package core

import (
	"context"
	"crypto/sha512"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"niceenv/internal/process"
)

func upstreamGet(ctx context.Context, address string) ([]byte, error) {
	u, err := url.Parse(address)
	if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil {
		return nil, invalid("版本源地址必须是 HTTPS 地址")
	}
	client := &http.Client{Timeout: 30 * time.Second, CheckRedirect: func(request *http.Request, via []*http.Request) error {
		if len(via) > 5 || request.URL.Scheme != "https" || request.URL.User != nil {
			return invalid("版本源重定向无效")
		}
		return nil
	}}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, address, nil)
	if err != nil {
		return nil, err
	}
	request.Header.Set("User-Agent", "NiceEnv/Go-Wails")
	response, err := client.Do(request)
	if err != nil || response.StatusCode == http.StatusForbidden || response.StatusCode == http.StatusTooManyRequests || response.StatusCode >= 500 {
		if response != nil {
			response.Body.Close()
		}
		transport := http.DefaultTransport.(*http.Transport).Clone()
		transport.Proxy = nil
		defer transport.CloseIdleConnections()
		client.Transport = transport
		response, err = client.Do(request.Clone(ctx))
	}
	if err != nil {
		return nil, &AppError{Code: "VERSION_NETWORK", Message: "无法连接官方版本目录"}
	}
	defer response.Body.Close()
	if response.StatusCode == http.StatusForbidden && u.Hostname() == "dev.mysql.com" {
		return mysqlCurlPage(ctx, address)
	}
	if response.StatusCode != http.StatusOK {
		return nil, &AppError{Code: "VERSION_HTTP", Message: "版本目录返回 " + response.Status}
	}
	const limit = 32 * 1024 * 1024
	data, err := io.ReadAll(io.LimitReader(response.Body, limit+1))
	if err != nil {
		return nil, err
	}
	if len(data) > limit {
		return nil, invalid("版本目录超过 32 MiB 限制")
	}
	return data, nil
}

func mysqlCurlPage(ctx context.Context, address string) ([]byte, error) {
	program, err := systemCurl()
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithTimeout(ctx, 35*time.Second)
	defer cancel()
	output := process.NewLimitedBuffer(4 * 1024 * 1024)
	stderr := process.NewLimitedBuffer(4096)
	child, err := process.Start(ctx, process.Spec{Program: program, Args: []string{"--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--proto-redir", "=https", "--max-time", "30", address}, Stdout: output, Stderr: stderr})
	if err != nil {
		return nil, err
	}
	result := child.Wait()
	if result.Err != nil || result.Code != 0 {
		return nil, &AppError{Code: "VERSION_SOURCE_UNAVAILABLE", Message: "MySQL 官方目录暂时不可访问", Detail: stderr.String()}
	}
	data := output.String()
	if len(data) > 4*1024*1024 {
		return nil, invalid("MySQL 官方目录超过大小限制")
	}
	return []byte(data), nil
}

type npmMetadata struct {
	Name    string            `json:"name"`
	Version string            `json:"version"`
	Bin     map[string]string `json:"bin"`
	Engines struct {
		Node string `json:"node"`
	} `json:"engines"`
	OptionalDependencies map[string]string `json:"optionalDependencies"`
	Dist                 struct {
		Tarball   string `json:"tarball"`
		Integrity string `json:"integrity"`
	} `json:"dist"`
}

func npmPackageMetadata(ctx context.Context, name, version string) (npmMetadata, error) {
	var meta npmMetadata
	data, err := upstreamGet(ctx, "https://registry.npmjs.org/"+name+"/"+url.PathEscape(version))
	if err != nil {
		return meta, err
	}
	if json.Unmarshal(data, &meta) != nil || meta.Name != name || meta.Version != version {
		return meta, invalid("官方包元信息与所选版本不一致")
	}
	return meta, nil
}
func npmPackageName(id, version, address string) (string, error) {
	if id != "npm" && id != "pnpm" || !safePart(version) {
		return "", invalid("包管理器标识无效")
	}
	names := []string{id}
	if id == "pnpm" {
		platform := runtime.GOOS
		if platform == "windows" {
			platform = "win32"
		}
		arch := runtime.GOARCH
		if arch == "amd64" {
			arch = "x64"
		}
		names = append(names, "@pnpm/exe."+platform+"-"+arch)
	}
	for _, name := range names {
		base := name
		if strings.HasPrefix(name, "@pnpm/") {
			base = strings.TrimPrefix(name, "@pnpm/")
		}
		if address == "https://registry.npmjs.org/"+name+"/-/"+base+"-"+version+".tgz" {
			return name, nil
		}
	}
	return "", &AppError{Code: "JS_CLI_INTEGRITY", Message: "包管理器下载地址不是当前平台的官方发行包"}
}
func verifyNPMArchive(ctx context.Context, id, version, address, sha, archive string) error {
	name, err := npmPackageName(id, version, address)
	if err != nil {
		return err
	}
	if decoded, err := hex.DecodeString(sha); err == nil && len(decoded) == 32 {
		return nil
	}
	meta, err := npmPackageMetadata(ctx, name, version)
	if err != nil {
		return err
	}
	if meta.Dist.Tarball != address {
		return invalid("官方包下载地址与清单不一致")
	}
	var expected []byte
	for _, item := range strings.Fields(meta.Dist.Integrity) {
		if strings.HasPrefix(item, "sha512-") {
			expected, err = base64.StdEncoding.DecodeString(strings.TrimPrefix(item, "sha512-"))
			if err == nil && len(expected) == sha512.Size {
				break
			}
		}
	}
	if len(expected) != sha512.Size {
		return &AppError{Code: "JS_CLI_INTEGRITY", Message: "官方目录缺少有效 SHA-512 校验值"}
	}
	file, err := os.Open(archive)
	if err != nil {
		return err
	}
	defer file.Close()
	hash := sha512.New()
	buffer := make([]byte, 128*1024)
	for {
		if err = ctx.Err(); err != nil {
			return err
		}
		n, readErr := file.Read(buffer)
		hash.Write(buffer[:n])
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			return readErr
		}
	}
	if subtle.ConstantTimeCompare(expected, hash.Sum(nil)) != 1 {
		return &AppError{Code: "JS_CLI_INTEGRITY", Message: "下载内容与官方 SHA-512 不一致，未安装"}
	}
	return nil
}
func prepareJSCLI(id, version, address, entry, root string) error {
	name, err := npmPackageName(id, version, address)
	if err != nil {
		return err
	}
	data, err := os.ReadFile(filepath.Join(root, "package", "package.json"))
	if err != nil {
		return err
	}
	var meta npmMetadata
	if len(data) > 1024*1024 || json.Unmarshal(data, &meta) != nil || meta.Name != name || meta.Version != version {
		return invalid("包管理器内容与官方包标识不一致")
	}
	if strings.HasPrefix(name, "@pnpm/exe.") {
		expected := "package/pnpm"
		if runtime.GOOS == "windows" {
			expected += ".exe"
		}
		if entry != expected {
			return invalid("pnpm 原生包入口无效")
		}
		if runtime.GOOS != "windows" {
			return os.Chmod(filepath.Join(root, filepath.FromSlash(entry)), 0755)
		}
		return nil
	}
	commands := map[string]string{id: "pnpm.cjs"}
	if id == "npm" {
		commands = map[string]string{"npm": "npm-cli.js", "npx": "npx-cli.js"}
	} else if entry == "package/bin/pnpm.mjs" {
		commands[id] = "pnpm.mjs"
	}
	if entry != "package/bin/"+commands[id] {
		return invalid("包管理器 JS 入口无效")
	}
	for command, script := range commands {
		if strings.TrimPrefix(meta.Bin[command], "./") != "bin/"+script {
			return invalid("包管理器命令映射与入口不一致")
		}
		file := filepath.Join(root, "package", "bin", script)
		if info, err := os.Lstat(file); err != nil || !info.Mode().IsRegular() || info.Size() == 0 {
			return invalid("包管理器缺少 " + script)
		}
		launcher := "#!/bin/sh\nexec node \"$(dirname \"$0\")/" + script + "\" \"$@\"\n"
		filename := command
		if runtime.GOOS == "windows" {
			filename += ".cmd"
			launcher = "@echo off\r\nnode \"%~dp0" + script + "\" %*\r\n"
		}
		if err = os.WriteFile(filepath.Join(root, "package", "bin", filename), []byte(launcher), 0755); err != nil {
			return err
		}
	}
	return nil
}
func nodeChecksum(ctx context.Context, version, address string) (string, error) {
	parts, ok := numericRuntimeVersion(version, false)
	if !ok || len(parts) != 3 {
		return "", invalid("Node.js 版本号无效")
	}
	prefix := "https://nodejs.org/dist/v" + version + "/"
	if !strings.HasPrefix(address, prefix) {
		return "", invalid("Node.js 校验源必须与官方发行目录一致")
	}
	filename := strings.TrimPrefix(address, prefix)
	if strings.ContainsAny(filename, "/\\?#") {
		return "", invalid("Node.js 下载文件名无效")
	}
	data, err := upstreamGet(ctx, prefix+"SHASUMS256.txt")
	if err != nil {
		return "", err
	}
	for _, line := range strings.Split(string(data), "\n") {
		fields := strings.Fields(line)
		if len(fields) != 2 || strings.TrimPrefix(fields[1], "*") != filename {
			continue
		}
		decoded, err := hex.DecodeString(fields[0])
		if err == nil && len(decoded) == 32 {
			return fields[0], nil
		}
	}
	return "", &AppError{Code: "CHECKSUM_NOT_FOUND", Message: fmt.Sprintf("官方校验文件缺少 %s", filename)}
}
