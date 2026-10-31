package core

import (
	"context"
	"encoding/hex"
	"fmt"
	"net/http"
	"net/url"
	"regexp"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

type upstreamObject map[string]any

func object(v any) upstreamObject { m, _ := v.(map[string]any); return m }
func array(v any) []any           { a, _ := v.([]any); return a }
func textValue(v any) string      { s, _ := v.(string); return s }
func field(v any, keys ...string) any {
	for _, key := range keys {
		v = object(v)[key]
	}
	return v
}
func textField(v any, keys ...string) string { return textValue(field(v, keys...)) }
func trueField(v any, key string) bool       { return field(v, key) == true }
func upstreamValue(ctx context.Context, address string) (any, error) {
	var v any
	err := upstreamJSON(ctx, address, &v)
	return v, err
}
func validHash(v any) string {
	s := textValue(v)
	decoded, err := hex.DecodeString(s)
	if err != nil || len(decoded) != 32 {
		return ""
	}
	return strings.ToLower(s)
}
func releaseVersion(template map[string]any, source versionSource, version, address string) remoteVersion {
	return remoteVersion{Version: version, URL: address, Entry: renderVersionEntry(template, source, version), Kind: archiveKind(address), Prerelease: prerelease(version)}
}
func parallelValues[T any](ctx context.Context, inputs []T, workers int, work func(T) bool) []T {
	keep := make([]bool, len(inputs))
	jobs := make(chan int)
	var wg sync.WaitGroup
	for worker := 0; worker < min(workers, len(inputs)); worker++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for index := range jobs {
				if ctx.Err() == nil {
					keep[index] = work(inputs[index])
				}
			}
		}()
	}
	for i := range inputs {
		jobs <- i
	}
	close(jobs)
	wg.Wait()
	out := []T{}
	for i, item := range inputs {
		if keep[i] {
			out = append(out, item)
		}
	}
	return out
}
func urlAvailable(ctx context.Context, address string) bool {
	client := &http.Client{Timeout: 15 * time.Second, CheckRedirect: func(req *http.Request, via []*http.Request) error {
		if len(via) > 5 || req.URL.Scheme != "https" || req.URL.User != nil {
			return invalid("下载地址重定向无效")
		}
		return nil
	}}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, address, nil)
	if err != nil {
		return false
	}
	request.Header.Set("Range", "bytes=0-0")
	request.Header.Set("User-Agent", "NiceEnv/Go-Wails")
	response, err := client.Do(request)
	if err != nil {
		return false
	}
	defer response.Body.Close()
	return response.StatusCode == http.StatusOK || response.StatusCode == http.StatusPartialContent
}

func fetchOtherVersions(ctx context.Context, template map[string]any, source versionSource) ([]remoteVersion, error) {
	out := []remoteVersion{}
	add := func(version, address string) *remoteVersion {
		out = append(out, releaseVersion(template, source, version, address))
		return &out[len(out)-1]
	}
	switch source.Kind {
	case "memcached":
		data, err := upstreamValue(ctx, "https://api.github.com/repos/nono303/memcached/tags?per_page=100")
		if err != nil {
			return nil, err
		}
		for _, row := range array(data) {
			version, commit := textField(row, "name"), textField(row, "commit", "sha")
			decoded, err := hex.DecodeString(commit)
			if !regexp.MustCompile(`^\d+\.\d+\.\d+$`).MatchString(version) || newerVersion("1.6.24", version) || err != nil || len(decoded) != 20 {
				continue
			}
			item := add(version, "https://github.com/nono303/memcached/archive/"+commit+".zip")
			item.Entry = "memcached-" + commit + "/libevent-2.1/x64/memcached.exe"
			item.Note = "Windows 社区构建 · Cygwin / SSE2"
		}
	case "composer":
		data, err := upstreamValue(ctx, "https://getcomposer.org/versions")
		if err != nil {
			return nil, err
		}
		for _, row := range array(field(data, "2")) {
			version, path := textField(row, "version"), textField(row, "path")
			if version != "" && strings.HasPrefix(path, "/") && !strings.HasPrefix(path, "//") {
				add(version, "https://getcomposer.org"+path)
			}
		}
	case "consul":
		data, err := upstreamValue(ctx, "https://releases.hashicorp.com/consul/index.json")
		if err != nil {
			return nil, err
		}
		for version, row := range object(field(data, "versions")) {
			if strings.Contains(version, "+") {
				continue
			}
			for _, file := range array(field(row, "builds")) {
				if textField(file, "os") == runtime.GOOS && textField(file, "arch") == runtime.GOARCH {
					add(version, textField(file, "url"))
				}
			}
		}
	case "gradle":
		data, err := upstreamValue(ctx, "https://services.gradle.org/versions/all")
		if err != nil {
			return nil, err
		}
		for _, row := range array(data) {
			if trueField(row, "snapshot") || trueField(row, "nightly") || trueField(row, "broken") {
				continue
			}
			item := add(textField(row, "version"), textField(row, "downloadUrl"))
			item.Prerelease = item.Prerelease || textField(row, "rcFor") != "" || textField(row, "milestoneFor") != ""
			item.SHA256 = validHash(field(row, "checksum"))
		}
	case "zig":
		data, err := upstreamValue(ctx, "https://ziglang.org/download/index.json")
		if err != nil {
			return nil, err
		}
		for version, row := range object(data) {
			if version == "master" {
				continue
			}
			file := field(row, "x86_64-windows")
			address := textField(file, "tarball")
			if address == "" {
				continue
			}
			item := add(version, address)
			parsed, err := url.Parse(address)
			if err != nil {
				continue
			}
			parts := strings.Split(parsed.Path, "/")
			item.Entry = strings.TrimSuffix(parts[len(parts)-1], ".zip") + "/zig.exe"
			item.SHA256 = validHash(field(file, "shasum"))
			item.SizeBytes, _ = strconv.ParseInt(textField(file, "size"), 10, 64)
		}
	case "dotnet":
		data, err := upstreamValue(ctx, "https://builds.dotnet.microsoft.com/dotnet/release-metadata/8.0/releases.json")
		if err != nil {
			return nil, err
		}
		for _, row := range array(field(data, "releases")) {
			sdks := append(array(field(row, "sdks")), field(row, "sdk"))
			for _, sdk := range sdks {
				for _, file := range array(field(sdk, "files")) {
					address := textField(file, "url")
					if textField(file, "rid") == "win-x64" && strings.HasSuffix(address, ".zip") {
						add(textField(sdk, "version"), address)
					}
				}
			}
		}
	case "flutter":
		data, err := upstreamValue(ctx, "https://storage.googleapis.com/flutter_infra_release/releases/releases_windows.json")
		if err != nil {
			return nil, err
		}
		for _, row := range array(field(data, "releases")) {
			arch := textField(row, "dart_sdk_arch")
			if textField(row, "channel") != "stable" || arch != "" && arch != "x64" {
				continue
			}
			item := add(textField(row, "version"), "https://storage.googleapis.com/flutter_infra_release/releases/"+textField(row, "archive"))
			item.SHA256 = validHash(field(row, "sha256"))
			item.ReleasedAt = textField(row, "release_date")
		}
	case "mongodb-tools", "mongodb":
		address := "https://downloads.mongodb.org/current.json"
		if source.Kind == "mongodb-tools" {
			address = "https://downloads.mongodb.org/tools/db/full.json"
		}
		data, err := upstreamValue(ctx, address)
		if err != nil {
			return nil, err
		}
		target, arch := runtime.GOOS, "x86_64"
		if target == "darwin" {
			target = "macos"
		}
		if runtime.GOARCH == "arm64" {
			arch = "arm64"
		}
		for _, row := range array(field(data, "versions")) {
			version := textField(row, "version")
			if trueField(row, "development_release") {
				continue
			}
			if source.Kind == "mongodb-tools" {
				parts, ok := numericRuntimeVersion(version, false)
				if !ok || len(parts) != 3 || parts[0] < 100 {
					continue
				}
			}
			for _, file := range array(field(row, "downloads")) {
				name := textField(file, "target")
				if source.Kind == "mongodb-tools" {
					name = textField(file, "name")
				} else if textField(file, "edition") != "base" {
					continue
				}
				if name != target || textField(file, "arch") != arch {
					continue
				}
				address := textField(file, "archive", "url")
				hash := validHash(field(file, "archive", "sha256"))
				if source.Kind == "mongodb-tools" && (hash == "" || address != fmt.Sprintf("https://fastdl.mongodb.org/tools/db/mongodb-database-tools-%s-%s-%s.zip", target, arch, version)) {
					continue
				}
				item := add(version, address)
				item.SHA256 = hash
			}
		}
	case "mariadb":
		data, err := upstreamValue(ctx, "https://downloads.mariadb.org/rest-api/mariadb/")
		if err != nil {
			return nil, err
		}
		branches := []string{}
		for _, row := range array(field(data, "major_releases")) {
			version := textField(row, "release_id")
			if textField(row, "release_status") == "Stable" && safePart(version) {
				branches = append(branches, version)
				if len(branches) == 8 {
					break
				}
			}
		}
		var mu sync.Mutex
		parallelValues(ctx, branches, 4, func(branch string) bool {
			data, err := upstreamValue(ctx, "https://downloads.mariadb.org/rest-api/mariadb/"+branch+"/")
			if err != nil {
				return false
			}
			mu.Lock()
			defer mu.Unlock()
			for version, row := range object(field(data, "releases")) {
				for _, file := range array(field(row, "files")) {
					name := textField(file, "file_name")
					if strings.HasSuffix(name, "-winx64.zip") && !strings.ContainsAny(name, "/\\") {
						item := add(version, "https://archive.mariadb.org/mariadb-"+version+"/winx64-packages/"+name)
						item.SHA256 = validHash(field(file, "checksum", "sha256sum"))
					}
				}
			}
			return true
		})
	case "phpmyadmin":
		data, err := upstreamValue(ctx, "https://www.phpmyadmin.net/home_page/version.json")
		if err != nil {
			return nil, err
		}
		for _, row := range array(field(data, "releases")) {
			version := textField(row, "version")
			if !regexp.MustCompile(`^5\.2\.\d+$`).MatchString(version) {
				continue
			}
			address := fmt.Sprintf("https://files.phpmyadmin.net/phpMyAdmin/%s/phpMyAdmin-%s-all-languages.zip", version, version)
			data, err := upstreamGet(ctx, address+".sha256")
			if err != nil {
				return nil, err
			}
			fields := strings.Fields(string(data))
			if len(fields) == 0 || validHash(fields[0]) == "" {
				return nil, invalid("phpMyAdmin 官方校验值无效")
			}
			add(version, address).SHA256 = validHash(fields[0])
		}
	case "mysql":
		osID, pattern := "3", `mysql-(\d+\.\d+\.\d+)-winx64\.zip`
		if runtime.GOOS == "darwin" {
			osID = "33"
			pattern = `mysql-(\d+\.\d+\.\d+)-macos\d+-x86_64\.tar\.gz`
			if runtime.GOARCH == "arm64" {
				pattern = `mysql-(\d+\.\d+\.\d+)-macos\d+-arm64\.tar\.gz`
			}
		}
		first, err := upstreamGet(ctx, "https://dev.mysql.com/downloads/mysql/?os="+osID)
		if err != nil {
			return nil, err
		}
		pages := [][]byte{first}
		var mu sync.Mutex
		branches := regexp.MustCompile(`<option value="(\d+\.\d+)"`).FindAllStringSubmatch(string(first), 5)
		parallelValues(ctx, branches, 3, func(branch []string) bool {
			data, err := upstreamGet(ctx, "https://dev.mysql.com/downloads/mysql/"+branch[1]+".html?os="+osID)
			if err != nil {
				return false
			}
			mu.Lock()
			pages = append(pages, data)
			mu.Unlock()
			return true
		})
		for _, page := range pages {
			for _, match := range regexp.MustCompile(pattern).FindAllStringSubmatch(string(page), -1) {
				version := match[1]
				index := strings.LastIndexByte(version, '.')
				if index < 0 {
					continue
				}
				item := add(version, "https://cdn.mysql.com/Downloads/MySQL-"+version[:index]+"/"+match[0])
				if runtime.GOOS == "darwin" {
					item.Entry = strings.TrimSuffix(match[0], ".tar.gz") + "/bin/mysqld"
				}
			}
		}
		file := "mysql-8.2.0-winx64.zip"
		if runtime.GOOS == "darwin" {
			arch := "x86_64"
			if runtime.GOARCH == "arm64" {
				arch = "arm64"
			}
			file = "mysql-8.2.0-macos13-" + arch + ".tar.gz"
		}
		item := add("8.2.0", "https://cdn.mysql.com/archives/mysql-8.2/"+file)
		item.Note = "Oracle 官方历史归档；8.2 已结束支持"
		if runtime.GOOS == "darwin" {
			item.Entry = strings.TrimSuffix(file, ".tar.gz") + "/bin/mysqld"
		}
	case "postgresql":
		data, err := upstreamGet(ctx, "https://www.enterprisedb.com/download-postgresql-binaries")
		if err != nil {
			return nil, err
		}
		version := regexp.MustCompile(`Version\s*(?:<!-- -->)?(\d+(?:\.\d+)+)`)
		link := regexp.MustCompile(`href="(https://sbp\.enterprisedb\.com/getfile\.jsp\?fileid=\d+)"[^>]*><img alt="Windows x86-64"`)
		for _, block := range strings.Split(string(data), "Binaries from installer")[1:] {
			v, u := version.FindStringSubmatch(block), link.FindStringSubmatch(block)
			if len(v) > 1 && len(u) > 1 {
				add(v[1], u[1]).Kind = "archive"
			}
		}
	case "apache":
		data, err := upstreamGet(ctx, "https://www.apachelounge.com/download/")
		if err != nil {
			return nil, err
		}
		matches := regexp.MustCompile(`(?i)\bhref\s*=\s*["']((?:https://www\.apachelounge\.com)?/download/VS(\d+)/binaries/httpd-(\d+\.\d+\.\d+)-(\d{6})-Win64-VS(\d+)\.zip)["']`).FindAllStringSubmatch(string(data), -1)
		sort.Slice(matches, func(i, j int) bool {
			if matches[i][4] != matches[j][4] {
				return matches[i][4] > matches[j][4]
			}
			return compareVersionNumber(matches[i][2], matches[j][2]) > 0
		})
		seen := map[string]bool{}
		for _, match := range matches {
			if match[2] != match[5] || seen[match[3]] {
				continue
			}
			seen[match[3]] = true
			address := match[1]
			if strings.HasPrefix(address, "/") {
				address = "https://www.apachelounge.com" + address
			}
			data, err := upstreamGet(ctx, address+".txt")
			if err != nil {
				return nil, err
			}
			parts := strings.Split(address, "/")
			marker := "SHA256-Checksum for: " + parts[len(parts)-1] + ":"
			lines := []string{}
			for _, line := range strings.Split(string(data), "\n") {
				if line = strings.TrimSpace(line); line != "" {
					lines = append(lines, line)
				}
			}
			hash := ""
			count := 0
			for i := 0; i < len(lines)-1; i++ {
				if lines[i] == marker {
					count++
					hash = validHash(lines[i+1])
				}
			}
			if count != 1 || hash == "" {
				return nil, invalid("Apache 官方 SHA256 校验文件无效")
			}
			add(match[3], address).SHA256 = hash
		}
	case "neo4j":
		data, err := upstreamGet(ctx, "https://neo4j.com/deployment-center/")
		if err != nil {
			return nil, err
		}
		for _, match := range regexp.MustCompile(`(neo4j-community-(\d+(?:\.\d+)+)-windows\.zip)`).FindAllStringSubmatch(string(data), -1) {
			add(match[2], "https://dist.neo4j.org/"+match[1])
		}
	case "tomcat":
		root := "https://downloads.apache.org/tomcat/"
		data, err := upstreamGet(ctx, root)
		if err != nil {
			return nil, err
		}
		for _, branch := range regexp.MustCompile(`href="(tomcat-\d+/)"`).FindAllStringSubmatch(string(data), 10) {
			page, err := upstreamGet(ctx, root+branch[1])
			if err != nil {
				return nil, err
			}
			for _, match := range regexp.MustCompile(`href="v(\d+\.\d+\.\d+)/"`).FindAllStringSubmatch(string(page), -1) {
				v := match[1]
				add(v, root+branch[1]+"v"+v+"/bin/apache-tomcat-"+v+"-windows-x64.zip")
			}
		}
	case "elasticsearch":
		data, err := upstreamValue(ctx, "https://artifacts-api.elastic.co/v1/versions")
		if err != nil {
			return nil, err
		}
		for _, raw := range array(field(data, "versions")) {
			version := textValue(raw)
			if safePart(version) {
				add(version, "https://artifacts.elastic.co/downloads/elasticsearch/elasticsearch-"+version+"-windows-x86_64.zip")
			}
		}
		out, err = normalizeVersions(out, source)
		if err != nil {
			return nil, err
		}
		out = parallelValues(ctx, out, 4, func(item remoteVersion) bool { return urlAvailable(ctx, item.URL) })
	case "rustup":
		data, err := upstreamGet(ctx, "https://static.rust-lang.org/rustup/release-stable.toml")
		if err != nil {
			return nil, err
		}
		match := regexp.MustCompile(`(?m)^version\s*=\s*['"]([\d.]+)['"]`).FindStringSubmatch(string(data))
		if len(match) > 1 {
			add(match[1], "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe").Note = "rustup installer"
		}
	case "python":
		data, err := upstreamGet(ctx, "https://www.python.org/ftp/python/")
		if err != nil {
			return nil, err
		}
		for _, match := range regexp.MustCompile(`>(3\.\d+\.\d+)/<`).FindAllStringSubmatch(string(data), -1) {
			v := match[1]
			add(v, "https://www.python.org/ftp/python/"+v+"/python-"+v+"-embed-amd64.zip")
		}
		out, err = normalizeVersions(out, source)
		if err != nil {
			return nil, err
		}
		out = parallelValues(ctx, out, 6, func(item remoteVersion) bool {
			index := strings.LastIndexByte(item.URL, '/')
			data, err := upstreamGet(ctx, item.URL[:index+1])
			return err == nil && strings.Contains(string(data), "\""+item.URL[index+1:]+"\"")
		})
	default:
		return nil, &AppError{Code: "UNKNOWN_VERSION_SOURCE", Message: "未知版本源类型：" + source.Kind}
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return out, nil
}
