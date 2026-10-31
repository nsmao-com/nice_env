package core

import (
	"context"
	"encoding/json"
	"fmt"
	"regexp"
	"runtime"
	"sort"
	"strings"
)

func builtinVersionSource(id string) versionSource {
	// Rules are the existing manifest defaults, kept independent of the old backend.
	github := map[string][4]string{
		"caddy":           {"caddyserver/caddy", "v", `caddy_.*_windows_amd64\.zip$`, "caddy.exe"},
		"frankenphp":      {"php/frankenphp", "v", `frankenphp-windows-x86_64\.zip$`, "frankenphp.exe"},
		"mailpit":         {"axllent/mailpit", "v", `mailpit-windows-amd64\.zip$`, "mailpit.exe"},
		"meilisearch":     {"meilisearch/meilisearch", "v", `meilisearch-windows-amd64\.exe$`, "meilisearch-windows-amd64.exe"},
		"minio":           {"minio/minio", "", `minio\.windows-amd64\..*\.exe$`, "minio.windows-amd64.{version}.exe"},
		"rustfs":          {"rustfs/rustfs", "", `rustfs-windows-x86_64.*\.zip$`, "rustfs.exe"},
		"qdrant":          {"qdrant/qdrant", "v", `qdrant-x86_64-pc-windows-msvc\.zip$`, "qdrant.exe"},
		"etcd":            {"etcd-io/etcd", "v", `etcd-v?[\d.]+-windows-amd64\.zip$`, "etcd-v{version}-windows-amd64/etcd.exe"},
		"sftpgo":          {"drakkan/sftpgo", "v", `sftpgo_v[\d.]+_windows_portable\.zip$`, "sftpgo.exe"},
		"rnacos":          {"nacos-group/r-nacos", "v", `rnacos-x86_64-pc-windows-msvc.*\.zip$`, "rnacos.exe"},
		"temporal-cli":    {"temporalio/cli", "v", `temporal_cli_[\d.]+_windows_amd64\.zip$`, "temporal.exe"},
		"cloudflared":     {"cloudflare/cloudflared", "", `cloudflared-windows-amd64\.exe$`, "cloudflared-windows-amd64.exe"},
		"coredns":         {"coredns/coredns", "v", `coredns_[\d.]+_windows_amd64\.zip$`, "coredns.exe"},
		"zincsearch":      {"zincsearch/zincsearch", "v", `zincsearch_[\d.]+_windows_x86_64\.tar\.gz$`, "zincsearch.exe"},
		"rabbitmq":        {"rabbitmq/rabbitmq-server", "v", `rabbitmq-server-windows-[\d.]+\.zip$`, "rabbitmq_server-{version}/sbin/rabbitmq-server.bat"},
		"ollama":          {"ollama/ollama", "v", `ollama-windows-amd64\.zip$`, "ollama.exe"},
		"mihomo":          {"MetaCubeX/mihomo", "v", `^mihomo-windows-amd64-v[\d.]+\.zip$`, "mihomo-windows-amd64.exe"},
		"adminer":         {"vrana/adminer", "v", `adminer-[\d.]+\.php$`, "adminer.php"},
		"k6":              {"grafana/k6", "v", `k6-v[\d.]+-windows-amd64\.zip$`, "k6-v{version}-windows-amd64/k6.exe"},
		"bun":             {"oven-sh/bun", "bun-v", `^bun-windows-x64\.zip$`, "bun-windows-x64/bun.exe"},
		"deno":            {"denoland/deno", "v", `^deno-x86_64-pc-windows-msvc\.zip$`, "deno.exe"},
		"erlang":          {"erlang/otp", "OTP-", `^otp_win64_[\d.]+\.zip$`, "bin/erl.exe"},
		"roadrunner":      {"roadrunner-server/roadrunner", "v", `^roadrunner-[\d.]+-windows-amd64\.zip$`, "roadrunner-{version}-windows-amd64/rr.exe"},
		"ruby":            {"oneclick/rubyinstaller2", "RubyInstaller-", `^rubyinstaller-[\d.]+-\d+-x64\.7z$`, "rubyinstaller-{version}-x64/bin/ruby.exe"},
		"ruby-devkit":     {"oneclick/rubyinstaller2", "RubyInstaller-", `^rubyinstaller-devkit-[\d.]+-\d+-x64\.exe$`, "{asset}"},
		"temurin-jdk21":   {"adoptium/temurin21-binaries", "jdk-", `^OpenJDK21U-jdk_x64_windows_hotspot_.*\.zip$`, "jdk-{version}/bin/java.exe"},
		"redis":           {"redis-windows/redis-windows", "", `^Redis-(?P<version>\d+\.\d+\.\d+)-Windows-x64-msys2\.zip$`, "Redis-{version}-Windows-x64-msys2/redis-server.exe"},
		"strawberry-perl": {"StrawberryPerl/Perl-Dist-Strawberry", "", `^strawberry-perl-[\d.]+-64bit-portable\.zip$`, "perl/bin/perl.exe"},
	}
	if rule, ok := github[id]; ok {
		return versionSource{Kind: "github", Repo: rule[0], TagPrefix: rule[1], AssetMatch: rule[2], EntryTemplate: rule[3], MaxVersions: 200}
	}
	kind := id
	if id == "node" {
		kind = "nodejs"
	}
	if id == "dotnet-sdk8" {
		kind = "dotnet"
	}
	if id == "rust" {
		kind = "rustup"
	}
	switch id {
	case "memcached", "consul", "composer", "gradle", "zig", "dotnet-sdk8", "flutter", "mongodb", "mysql", "mariadb", "postgresql", "apache", "tomcat", "elasticsearch", "neo4j", "rust", "node", "php", "go", "nginx", "python", "phpmyadmin":
		source := versionSource{Kind: kind, MaxVersions: 200}
		if id == "php" {
			source.VersionFilter = `^(?:[7-9]|[1-9]\d+)\.`
		}
		if id == "python" {
			source.VersionFilter = `^3\.(?:9|[1-9]\d+)\.`
		}
		return source
	}
	return versionSource{}
}
func upstreamJSON(ctx context.Context, address string, target any) error {
	data, err := upstreamGet(ctx, address)
	if err != nil {
		return err
	}
	if json.Unmarshal(data, target) != nil {
		return invalid("官方版本目录返回格式无效")
	}
	return nil
}
func fetchGitHubVersions(ctx context.Context, template map[string]any, source versionSource) ([]remoteVersion, error) {
	parts := strings.Split(source.Repo, "/")
	if len(parts) != 2 || !safePart(parts[0]) || !safePart(parts[1]) {
		return nil, invalid("GitHub 仓库必须为 owner/name")
	}
	matcher, err := regexp.Compile(source.AssetMatch)
	if err != nil || source.AssetMatch == "" {
		return nil, invalid("GitHub 下载文件匹配规则无效")
	}
	var strip *regexp.Regexp
	if source.VersionStrip != "" {
		strip, err = regexp.Compile(source.VersionStrip)
		if err != nil {
			return nil, invalid("GitHub 版本提取规则无效")
		}
	}
	type asset struct {
		Name   string `json:"name"`
		URL    string `json:"browser_download_url"`
		Digest string `json:"digest"`
		Size   int64  `json:"size"`
	}
	type release struct {
		Tag        string  `json:"tag_name"`
		Draft      bool    `json:"draft"`
		Prerelease bool    `json:"prerelease"`
		Published  string  `json:"published_at"`
		Assets     []asset `json:"assets"`
	}
	out := []remoteVersion{}
	pages := (source.MaxVersions + 99) / 100
	pages = max(1, min(10, pages))
	for page := 1; page <= pages; page++ {
		var rows []release
		if err = upstreamJSON(ctx, fmt.Sprintf("https://api.github.com/repos/%s/releases?per_page=100&page=%d", source.Repo, page), &rows); err != nil {
			return nil, err
		}
		for _, row := range rows {
			if row.Draft || row.Tag == "" || row.Prerelease && !source.IncludePrerelease {
				continue
			}
			version := strings.TrimPrefix(row.Tag, source.TagPrefix)
			if strip != nil {
				match := strip.FindStringSubmatch(version)
				if len(match) > 1 {
					version = match[1]
				}
			}
			assets := []asset{}
			for _, a := range row.Assets {
				if !matcher.MatchString(a.Name) || a.URL == "" {
					continue
				}
				wrong := []string{"arm64", "aarch64", "armv7", "armhf"}
				if runtime.GOARCH == "arm64" {
					wrong = []string{"x86_64", "amd64", "x64", "i386", "i686", "386"}
				}
				matches := true
				for _, marker := range wrong {
					if strings.Contains(strings.ToLower(a.Name), marker) {
						matches = false
					}
				}
				if matches {
					assets = append(assets, a)
				}
			}
			sort.Slice(assets, func(i, j int) bool {
				a, b := assets[i], assets[j]
				if (a.Digest != "") != (b.Digest != "") {
					return a.Digest != ""
				}
				if a.Size != b.Size {
					return a.Size > b.Size
				}
				return a.Name < b.Name
			})
			if len(assets) == 0 {
				continue
			}
			a := assets[0]
			pre := row.Prerelease || prerelease(version)
			if index := matcher.SubexpIndex("version"); index >= 0 {
				match := matcher.FindStringSubmatch(a.Name)
				if index < len(match) {
					version = match[index]
				}
			}
			if template["id"] == "strawberry-perl" {
				version = strings.TrimSuffix(strings.TrimPrefix(a.Name, "strawberry-perl-"), "-64bit-portable.zip")
			}
			hash := ""
			if strings.HasPrefix(a.Digest, "sha256:") {
				hash = strings.TrimPrefix(a.Digest, "sha256:")
			}
			out = append(out, remoteVersion{Version: version, URL: a.URL, SHA256: hash, SizeBytes: a.Size, Entry: strings.ReplaceAll(renderVersionEntry(template, source, version), "{asset}", a.Name), Kind: archiveKind(a.URL), Prerelease: pre, ReleasedAt: row.Published})
		}
		if len(rows) < 100 {
			break
		}
	}
	return out, nil
}
func (c *Core) fetchNodeVersions(ctx context.Context, template map[string]any, source versionSource) ([]remoteVersion, error) {
	var rows []struct {
		Version string          `json:"version"`
		Files   []string        `json:"files"`
		LTS     json.RawMessage `json:"lts"`
		Date    string          `json:"date"`
	}
	if err := upstreamJSON(ctx, "https://nodejs.org/dist/index.json", &rows); err != nil {
		return nil, err
	}
	arch := runtime.GOARCH
	if arch == "amd64" {
		arch = "x64"
	}
	platform, key, ext := "win-"+arch, "win-"+arch+"-zip", "zip"
	if runtime.GOOS == "darwin" {
		platform = "darwin-" + arch
		key = "osx-" + arch + "-tar"
		ext = "tar.gz"
	}
	out := []remoteVersion{}
	aliases := map[string]string{}
	for _, row := range rows {
		version := strings.TrimPrefix(row.Version, "v")
		parts, ok := numericRuntimeVersion(version, false)
		if !ok || len(parts) != 3 {
			continue
		}
		var lts string
		_ = json.Unmarshal(row.LTS, &lts)
		if regexp.MustCompile(`^[A-Za-z-]{1,64}$`).MatchString(lts) {
			for _, alias := range []string{"*", strings.ToLower(lts)} {
				if aliases[alias] == "" || newerVersion(version, aliases[alias]) {
					aliases[alias] = version
				}
			}
		}
		available := false
		for _, file := range row.Files {
			available = available || file == key
		}
		if !available {
			continue
		}
		note := ""
		if lts != "" {
			note = lts + " LTS"
		}
		out = append(out, remoteVersion{Version: version, URL: fmt.Sprintf("https://nodejs.org/dist/v%s/node-v%s-%s.%s", version, version, platform, ext), Entry: renderVersionEntry(template, source, version), Kind: archiveKind(ext), Note: note, ReleasedAt: row.Date})
		out[len(out)-1].Kind = "archive"
		if ext == "tar.gz" {
			out[len(out)-1].Kind = "targz"
		}
	}
	if len(aliases) == 0 {
		return nil, invalid("Node.js 官方索引缺少 LTS 信息，保留旧缓存")
	}
	data, _ := json.Marshal(aliases)
	c.siteChanges.Lock()
	err := c.Store.saveSetting(ctx, "nodeLtsAliases:v1", string(data))
	c.siteChanges.Unlock()
	return out, err
}
func fetchNPMVersions(ctx context.Context, template map[string]any, source versionSource) ([]remoteVersion, error) {
	id, _ := template["id"].(string)
	if id != "npm" && id != "pnpm" {
		return nil, invalid("npm 版本源仅支持 npm/pnpm")
	}
	var index struct {
		Versions map[string]npmMetadata `json:"versions"`
		Time     map[string]string      `json:"time"`
	}
	if err := upstreamJSON(ctx, "https://registry.npmjs.org/"+id, &index); err != nil {
		return nil, err
	}
	platform, arch := runtime.GOOS, runtime.GOARCH
	if platform == "windows" {
		platform = "win32"
	}
	if arch == "amd64" {
		arch = "x64"
	}
	native := "@pnpm/exe." + platform + "-" + arch
	out := []remoteVersion{}
	for version, meta := range index.Versions {
		if !safePart(version) || meta.Version != version || meta.Name != id {
			continue
		}
		address := "https://registry.npmjs.org/" + id + "/-/" + id + "-" + version + ".tgz"
		if meta.Dist.Tarball != address {
			continue
		}
		entry := "package/bin/npm-cli.js"
		if id == "pnpm" && meta.OptionalDependencies[native] == version {
			address = "https://registry.npmjs.org/" + native + "/-/exe." + platform + "-" + arch + "-" + version + ".tgz"
			entry = "package/pnpm"
			if platform == "win32" {
				entry += ".exe"
			}
		} else {
			if id == "pnpm" {
				entry = "package/bin/pnpm.cjs"
				if meta.Bin[id] == "bin/pnpm.mjs" {
					entry = "package/bin/pnpm.mjs"
				}
			}
			if strings.TrimPrefix(meta.Bin[id], "./") != strings.TrimPrefix(entry, "package/") {
				continue
			}
		}
		note := ""
		if meta.Engines.Node != "" {
			note = "Node.js " + meta.Engines.Node
		}
		out = append(out, remoteVersion{Version: version, URL: address, Entry: entry, Kind: "targz", Prerelease: prerelease(version), Note: note, ReleasedAt: index.Time[version]})
	}
	return out, nil
}
func fetchGoVersions(ctx context.Context, template map[string]any, source versionSource) ([]remoteVersion, error) {
	var rows []struct {
		Version string `json:"version"`
		Stable  bool   `json:"stable"`
		Files   []struct {
			OS       string `json:"os"`
			Arch     string `json:"arch"`
			Kind     string `json:"kind"`
			Filename string `json:"filename"`
			SHA256   string `json:"sha256"`
			Size     int64  `json:"size"`
		} `json:"files"`
	}
	if err := upstreamJSON(ctx, "https://go.dev/dl/?mode=json&include=all", &rows); err != nil {
		return nil, err
	}
	out := []remoteVersion{}
	for _, row := range rows {
		version := strings.TrimPrefix(row.Version, "go")
		for _, file := range row.Files {
			if file.OS != runtime.GOOS || file.Arch != runtime.GOARCH || file.Kind != "archive" {
				continue
			}
			out = append(out, remoteVersion{Version: version, URL: "https://go.dev/dl/" + file.Filename, SHA256: file.SHA256, SizeBytes: file.Size, Entry: renderVersionEntry(template, source, version), Kind: archiveKind(file.Filename), Prerelease: !row.Stable})
			break
		}
	}
	return out, nil
}
func fetchWebVersions(ctx context.Context, template map[string]any, source versionSource) ([]remoteVersion, error) {
	bases := []string{"https://nginx.org/download/"}
	pattern := `nginx-(\d+\.\d+\.\d+)\.zip`
	if source.Kind == "php" {
		bases = []string{"https://downloads.php.net/~windows/releases/", "https://downloads.php.net/~windows/releases/archives/"}
		pattern = `php-(\d+\.\d+\.\d+)-Win32-(?:vs\d+|vc\d+)-x64\.zip`
	}
	out := []remoteVersion{}
	matcher := regexp.MustCompile(pattern)
	var lastErr error
	for _, base := range bases {
		data, err := upstreamGet(ctx, base)
		if source.Kind == "nginx" && (err != nil || !strings.Contains(string(data), ".zip")) {
			data, err = upstreamGet(ctx, "https://nginx.org/en/download.html")
		}
		if err != nil {
			lastErr = err
			continue
		}
		for _, match := range matcher.FindAllStringSubmatch(string(data), -1) {
			out = append(out, remoteVersion{Version: match[1], URL: base + match[0], Entry: renderVersionEntry(template, source, match[1]), Kind: "archive"})
		}
	}
	if len(out) == 0 && lastErr != nil {
		return nil, lastErr
	}
	return out, nil
}
