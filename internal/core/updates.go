package core

import (
	"context"
	"crypto/sha256"
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

	"github.com/Masterminds/semver/v3"
)

const releasesPage = "https://github.com/nsmao-com/nice_env/releases"
const releasesAPI = "https://api.github.com/repos/nsmao-com/nice_env/releases/latest"

type appRelease struct {
	Tag         string  `json:"tag"`
	HTMLURL     string  `json:"htmlUrl"`
	Body        string  `json:"body"`
	PublishedAt string  `json:"publishedAt"`
	AssetName   *string `json:"assetName"`
	AssetURL    *string `json:"assetUrl"`
	AssetSize   *int64  `json:"assetSize"`
	digest      string
}
type updateReceipt struct {
	FileName string `json:"fileName"`
	Version  string `json:"version"`
	URL      string `json:"url"`
	Size     int64  `json:"size"`
	SHA256   string `json:"sha256"`
}

func updateAssetMatches(name string) bool {
	n := strings.ToLower(name)
	arm := strings.Contains(n, "arm64") || strings.Contains(n, "aarch64")
	x64 := strings.Contains(n, "x64") || strings.Contains(n, "x86_64") || strings.Contains(n, "amd64")
	if runtime.GOARCH == "arm64" && !arm || runtime.GOARCH == "amd64" && (arm || !x64) {
		return false
	}
	if runtime.GOOS == "windows" {
		return strings.HasSuffix(n, ".msi") || strings.HasSuffix(n, "setup.exe") || strings.HasSuffix(n, "installer.exe")
	}
	return runtime.GOOS == "darwin" && strings.HasSuffix(n, ".dmg")
}

func latestAppRelease(ctx context.Context) (*appRelease, error) {
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, "GET", releasesAPI, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", "NiceEnv")
	req.Header.Set("Accept", "application/vnd.github+json")
	transport := outboundTransport()
	defer transport.CloseIdleConnections()
	client := http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	response, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode == 404 {
		return nil, nil
	}
	if response.StatusCode != 200 {
		return nil, fmt.Errorf("release HTTP %d", response.StatusCode)
	}
	data, err := io.ReadAll(io.LimitReader(response.Body, 2*1024*1024+1))
	if err != nil || len(data) > 2*1024*1024 {
		return nil, invalid("发布信息读取失败或过大")
	}
	var release struct {
		Tag         string `json:"tag_name"`
		HTMLURL     string `json:"html_url"`
		Body        string `json:"body"`
		PublishedAt string `json:"published_at"`
		Draft       bool   `json:"draft"`
		Prerelease  bool   `json:"prerelease"`
		Assets      []struct {
			Name   string `json:"name"`
			URL    string `json:"browser_download_url"`
			Size   int64  `json:"size"`
			Digest string `json:"digest"`
		} `json:"assets"`
	}
	if json.Unmarshal(data, &release) != nil {
		return nil, invalid("发布信息格式无效")
	}
	version, err := semver.StrictNewVersion(canonicalVersion(release.Tag))
	if err != nil || release.Draft || release.Prerelease || version.Prerelease() != "" {
		return nil, invalid("不是有效正式版本")
	}
	out := &appRelease{Tag: release.Tag, HTMLURL: releasesPage, Body: release.Body, PublishedAt: release.PublishedAt}
	if strings.HasPrefix(release.HTMLURL, releasesPage+"/tag/") {
		out.HTMLURL = release.HTMLURL
	}
	for _, asset := range release.Assets {
		if !updateAssetMatches(asset.Name) || !safeUpdateName(asset.Name) || !strings.HasPrefix(asset.URL, "https://github.com/nsmao-com/nice_env/releases/download/") || asset.Size <= 0 {
			continue
		}
		out.AssetName = &asset.Name
		out.AssetURL = &asset.URL
		out.AssetSize = &asset.Size
		out.digest = strings.TrimPrefix(asset.Digest, "sha256:")
		break
	}
	return out, nil
}
func safeUpdateName(name string) bool {
	return name != "" && len(name) <= 200 && name == filepath.Base(name) && !strings.ContainsAny(name, "/\\:*?\"<>|\x00\r\n") && !strings.HasSuffix(name, ".") && !strings.HasSuffix(name, " ")
}
func (c *Core) UpdateDirectory() (string, error) {
	dir := filepath.Join(c.Home, "updates")
	return dir, managedDirectory(c.Home, dir)
}
func (c *Core) registerUpdates() {
	c.handle("check_updates", func(ctx context.Context, _ json.RawMessage) (any, error) {
		out := map[string]any{"appVersion": canonicalVersion(c.Version), "latestVersion": nil, "releaseUrl": releasesPage, "manifestRevision": c.catalog.Revision, "manifestUpdate": nil, "appUpdate": nil, "release": nil}
		release, err := latestAppRelease(ctx)
		if err == nil {
			out["appUpdate"] = false
			out["latestVersion"] = canonicalVersion(c.Version)
			if release != nil {
				out["latestVersion"] = canonicalVersion(release.Tag)
				out["releaseUrl"] = release.HTMLURL
				out["release"] = release
				remote, e1 := semver.StrictNewVersion(canonicalVersion(release.Tag))
				local, e2 := semver.NewVersion(canonicalVersion(c.Version))
				if e1 == nil && e2 == nil {
					out["appUpdate"] = remote.GreaterThan(local) && remote.Major() >= 2
				}
			}
		}
		address, _, err := c.Store.setting(ctx, "manifestUrl")
		if err != nil {
			return nil, err
		}
		if address != "" {
			if _, catalog, e := fetchManifest(ctx, address); e == nil {
				out["manifestUpdate"] = catalog.Revision > c.catalog.Revision
			}
		}
		return out, ctx.Err()
	})
	c.handle("download_update", c.downloadUpdate)
}
func (c *Core) downloadUpdate(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		URL       string `json:"url"`
		Version   string `json:"version"`
		AssetName string `json:"assetName"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.updateDownload.TryLock() {
		return nil, invalid("更新包正在下载，请稍后重试")
	}
	defer c.updateDownload.Unlock()
	release, err := latestAppRelease(ctx)
	if err != nil {
		return nil, &AppError{Code: "UPDATE_RELEASE_UNAVAILABLE", Message: "无法核对最新正式版，请重新检查更新"}
	}
	if release == nil || release.AssetURL == nil || release.AssetName == nil || release.AssetSize == nil || in.URL != *release.AssetURL || canonicalVersion(in.Version) != canonicalVersion(release.Tag) || in.AssetName != "" && in.AssetName != *release.AssetName {
		return nil, invalid("下载地址与本平台正式发布包不匹配，请重新检查更新")
	}
	version, err := semver.StrictNewVersion(canonicalVersion(release.Tag))
	if err != nil || version.Major() < 2 {
		return nil, invalid("旧版 Tauri 安装包不能覆盖 Wails 版本")
	}
	dir, err := c.UpdateDirectory()
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Minute)
	defer cancel()
	transport := outboundTransport()
	defer transport.CloseIdleConnections()
	client := http.Client{Transport: transport, CheckRedirect: func(req *http.Request, via []*http.Request) error {
		if len(via) > 5 || req.URL.Scheme != "https" || req.URL.User != nil {
			return invalid("更新下载重定向无效")
		}
		return nil
	}}
	request, err := http.NewRequestWithContext(ctx, "GET", in.URL, nil)
	if err != nil {
		return nil, err
	}
	request.Header.Set("User-Agent", "NiceEnv")
	response, err := client.Do(request)
	if err != nil {
		return nil, &AppError{Code: "UPDATE_DOWNLOAD_FAILED", Message: "无法下载更新包，请检查网络后重试"}
	}
	defer response.Body.Close()
	if response.StatusCode != 200 {
		return nil, invalid("更新服务器返回 " + response.Status)
	}
	if *release.AssetSize > 2*1024*1024*1024 {
		return nil, invalid("更新包超过 2 GiB 限制")
	}
	file, err := os.CreateTemp(dir, ".download-*")
	if err != nil {
		return nil, err
	}
	defer os.Remove(file.Name())
	defer file.Close()
	hash := sha256.New()
	begin, last := time.Now(), time.Now()
	received := int64(0)
	buf := make([]byte, 64*1024)
	emit := func(state string) {
		if c.emit == nil {
			return
		}
		speed := float64(received) / max(time.Since(begin).Seconds(), 0.001)
		eta := float64(max(0, *release.AssetSize-received)) / max(speed, 1)
		if state == "downloaded" {
			speed, eta = 0, 0
		}
		c.emit("update://progress", map[string]any{"received": received, "total": *release.AssetSize, "speedBps": int64(speed), "etaSec": eta, "state": state})
	}
	for {
		n, readErr := response.Body.Read(buf)
		if n > 0 {
			received += int64(n)
			if received > *release.AssetSize {
				return nil, invalid("更新包大小与发布记录不一致")
			}
			if _, err = file.Write(buf[:n]); err != nil {
				return nil, err
			}
			hash.Write(buf[:n])
			if time.Since(last) >= 200*time.Millisecond {
				emit("downloading")
				last = time.Now()
			}
		}
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			return nil, readErr
		}
		if err = ctx.Err(); err != nil {
			return nil, err
		}
	}
	digest := hex.EncodeToString(hash.Sum(nil))
	if received != *release.AssetSize || release.digest != "" && !strings.EqualFold(digest, release.digest) {
		return nil, invalid("更新包大小或 SHA-256 与发布记录不一致")
	}
	if err = file.Sync(); err != nil {
		return nil, err
	}
	if err = file.Close(); err != nil {
		return nil, err
	}
	dest := filepath.Join(dir, *release.AssetName)
	if info, e := os.Lstat(dest); e == nil && !info.Mode().IsRegular() {
		return nil, invalid("更新目标不是普通文件")
	}
	if err = os.Rename(file.Name(), dest); err != nil {
		return nil, err
	}
	receipt := updateReceipt{FileName: *release.AssetName, Version: canonicalVersion(release.Tag), URL: in.URL, Size: received, SHA256: digest}
	encoded, err := json.Marshal(receipt)
	if err != nil {
		return nil, err
	}
	if err = atomicWrite(dest+".receipt.json", encoded); err != nil {
		return nil, err
	}
	emit("downloaded")
	return map[string]any{"path": Path(dest), "fileName": receipt.FileName, "sizeBytes": received}, nil
}

// Validate again immediately before launch; callers cannot execute an arbitrary path.
func (c *Core) ValidateUpdateInstaller(ctx context.Context, path string) (string, error) {
	dir, err := c.UpdateDirectory()
	if err != nil {
		return "", err
	}
	path = DisplayPath(path)
	if !filepath.IsAbs(path) || projectPathKey(filepath.Dir(path)) != projectPathKey(dir) || !safeUpdateName(filepath.Base(path)) || !updateAssetMatches(filepath.Base(path)) {
		return "", invalid("请选择本应用下载目录中的对应平台安装包")
	}
	info, err := os.Lstat(path)
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() {
		return "", invalid("更新包不能是链接或目录")
	}
	receiptInfo, err := os.Lstat(path + ".receipt.json")
	if err != nil {
		return "", invalid("更新包缺少校验记录，请重新下载")
	}
	if !receiptInfo.Mode().IsRegular() || receiptInfo.Size() > 8192 {
		return "", invalid("更新校验记录无效")
	}
	raw, err := os.ReadFile(path + ".receipt.json")
	if err != nil {
		return "", err
	}
	var receipt updateReceipt
	if json.Unmarshal(raw, &receipt) != nil || receipt.FileName != filepath.Base(path) || receipt.Size != info.Size() {
		return "", invalid("更新包记录不匹配，请重新下载")
	}
	u, err := url.Parse(receipt.URL)
	if err != nil || u.Scheme != "https" || u.Hostname() != "github.com" || !strings.HasPrefix(u.Path, "/nsmao-com/nice_env/releases/download/") {
		return "", invalid("更新包来源无效")
	}
	version, err := semver.StrictNewVersion(receipt.Version)
	if err != nil || version.Major() < 2 {
		return "", invalid("更新包版本无效")
	}
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hash := sha256.New()
	buf := make([]byte, 64*1024)
	for {
		if err = ctx.Err(); err != nil {
			return "", err
		}
		n, e := file.Read(buf)
		hash.Write(buf[:n])
		if e == io.EOF {
			break
		}
		if e != nil {
			return "", e
		}
	}
	if hex.EncodeToString(hash.Sum(nil)) != receipt.SHA256 {
		return "", invalid("安装包已变化，请重新下载")
	}
	return path, nil
}
