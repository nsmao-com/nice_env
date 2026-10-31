package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"net/url"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"
)

type versionSource struct {
	Kind              string `json:"kind"`
	Repo              string `json:"repo,omitempty"`
	AssetMatch        string `json:"assetMatch,omitempty"`
	TagPrefix         string `json:"tagPrefix,omitempty"`
	URLTemplate       string `json:"urlTemplate,omitempty"`
	EntryTemplate     string `json:"entryTemplate,omitempty"`
	VersionFilter     string `json:"versionFilter,omitempty"`
	VersionStrip      string `json:"versionStrip,omitempty"`
	MaxVersions       int    `json:"maxVersions,omitempty"`
	IncludePrerelease bool   `json:"includePrerelease,omitempty"`
}
type remoteVersion struct {
	Version    string `json:"version"`
	URL        string `json:"url"`
	SHA256     string `json:"sha256,omitempty"`
	SizeBytes  int64  `json:"sizeBytes,omitempty"`
	Entry      string `json:"entry"`
	Kind       string `json:"kind"`
	Prerelease bool   `json:"prerelease"`
	Note       string `json:"note,omitempty"`
	ReleasedAt string `json:"releasedAt,omitempty"`
}
type versionCatalog struct {
	ID       string          `json:"id"`
	Remote   []remoteVersion `json:"remote"`
	Online   bool            `json:"online"`
	CachedAt int64           `json:"cachedAt,omitempty"`
	Error    string          `json:"error,omitempty"`
}
type versionManager struct {
	mu    sync.Mutex
	gates map[string]chan struct{}
	slots chan struct{}
}

func (c *Core) versionTemplate(id string) (map[string]any, error) { return c.findPackage(id) }
func sourceFor(template map[string]any) (versionSource, error) {
	var source versionSource
	if raw, ok := template["versionSource"]; ok {
		data, err := json.Marshal(raw)
		if err != nil {
			return source, err
		}
		err = json.Unmarshal(data, &source)
		return source, err
	}
	id, _ := template["id"].(string)
	if runtime.GOOS == "darwin" && id != "composer" && id != "mongodb" && id != "mysql" {
		return source, nil
	}
	source = builtinVersionSource(id)
	return source, nil
}
func versionCacheKey(template map[string]any, source versionSource) string {
	payload, _ := json.Marshal([]any{source, template["os"], template["arch"], template["entry"], template["kind"], runtime.GOOS, runtime.GOARCH})
	digest := sha256.Sum256(payload)
	return "versionCatalog:go-v1:" + template["id"].(string) + ":" + hex.EncodeToString(digest[:])
}
func (c *Core) readVersionCache(ctx context.Context, key, id string) (versionCatalog, error) {
	out := versionCatalog{ID: id, Remote: []remoteVersion{}}
	raw, exists, err := c.Store.setting(ctx, key)
	if err != nil {
		return out, err
	}
	if exists && json.Unmarshal([]byte(raw), &out) != nil {
		out = versionCatalog{ID: id, Remote: []remoteVersion{}}
	}
	if out.Remote == nil {
		out.Remote = []remoteVersion{}
	}
	out.Online = false
	return out, nil
}
func (c *Core) versionCatalog(ctx context.Context, id string, force, cachedOnly bool) (versionCatalog, error) {
	out := versionCatalog{ID: id, Remote: []remoteVersion{}}
	template, err := c.versionTemplate(id)
	if err != nil {
		return out, err
	}
	source, err := sourceFor(template)
	if err != nil {
		return out, err
	}
	if source.Kind == "" || source.Kind == "static" {
		return out, nil
	}
	key := versionCacheKey(template, source)
	out, err = c.readVersionCache(ctx, key, id)
	if err != nil {
		return out, err
	}
	fresh := func(cat versionCatalog) bool {
		age := time.Now().UnixMilli() - cat.CachedAt
		return cat.CachedAt > 0 && age >= 0 && age < int64(6*time.Hour/time.Millisecond)
	}
	if cachedOnly || !force && fresh(out) {
		return out, nil
	}
	requested := time.Now().UnixMilli()
	c.versions.mu.Lock()
	gate := c.versions.gates[key]
	if gate == nil {
		gate = make(chan struct{}, 1)
		c.versions.gates[key] = gate
	}
	c.versions.mu.Unlock()
	select {
	case gate <- struct{}{}:
	case <-ctx.Done():
		return out, ctx.Err()
	}
	defer func() { <-gate }()
	out, err = c.readVersionCache(ctx, key, id)
	if err != nil {
		return out, err
	}
	if !force && fresh(out) || force && out.CachedAt >= requested {
		return out, nil
	}
	select {
	case c.versions.slots <- struct{}{}:
	case <-ctx.Done():
		return out, ctx.Err()
	}
	defer func() { <-c.versions.slots }()
	list, fetchErr := c.fetchVersions(ctx, template, source)
	if fetchErr == nil {
		list, fetchErr = normalizeVersions(list, source)
	}
	if fetchErr == nil && len(list) == 0 {
		fetchErr = &AppError{Code: "EMPTY_VERSION_CATALOG", Message: "上游没有返回适用于此平台的版本"}
	}
	if fetchErr != nil {
		out.Error = publicError(fetchErr).Message
		if len(out.Remote) == 0 {
			out.Error += "；当前显示内置版本"
		} else {
			out.Error += "；显示上次缓存"
		}
		return out, nil
	}
	out = versionCatalog{ID: id, Remote: list, Online: true, CachedAt: time.Now().UnixMilli()}
	data, err := json.Marshal(out)
	if err != nil {
		return out, err
	}
	if err = c.Store.saveSetting(ctx, key, string(data)); err != nil {
		return out, err
	}
	return out, nil
}
func normalizeVersions(list []remoteVersion, source versionSource) ([]remoteVersion, error) {
	var filter *regexp.Regexp
	var err error
	if source.VersionFilter != "" {
		filter, err = regexp.Compile(source.VersionFilter)
		if err != nil {
			return nil, invalid("版本筛选规则无效")
		}
	}
	out := []remoteVersion{}
	for _, item := range list {
		if !safePart(item.Version) || !source.IncludePrerelease && item.Prerelease || filter != nil && !filter.MatchString(item.Version) {
			continue
		}
		u, err := url.Parse(item.URL)
		if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil {
			continue
		}
		if item.SHA256 != "" {
			decoded, err := hex.DecodeString(item.SHA256)
			if err != nil || len(decoded) != 32 {
				continue
			}
		}
		out = append(out, item)
	}
	sort.SliceStable(out, func(i, j int) bool {
		if out[i].Version == out[j].Version {
			return out[i].SHA256 != "" && out[j].SHA256 == ""
		}
		return newerVersion(out[i].Version, out[j].Version)
	})
	seen := map[string]bool{}
	unique := []remoteVersion{}
	limit := source.MaxVersions
	if limit <= 0 {
		limit = 60
	}
	limit = min(limit, 1000)
	for _, item := range out {
		if seen[item.Version] {
			continue
		}
		seen[item.Version] = true
		unique = append(unique, item)
		if len(unique) >= limit {
			break
		}
	}
	return unique, nil
}
func renderVersionEntry(template map[string]any, source versionSource, version string) string {
	entry := source.EntryTemplate
	if entry == "" {
		entry, _ = template["entry"].(string)
		old, _ := template["version"].(string)
		if old != "" {
			entry = strings.ReplaceAll(entry, old, version)
		}
	}
	return strings.ReplaceAll(entry, "{version}", version)
}
func archiveKind(address string) string {
	if u, err := url.Parse(address); err == nil {
		address = u.Path
	}
	address = strings.ToLower(address)
	switch {
	case strings.HasSuffix(address, ".zip"):
		return "archive"
	case strings.HasSuffix(address, ".7z"):
		return "sevenzip"
	case strings.HasSuffix(address, ".gz"):
		return "targz"
	default:
		return "binary"
	}
}
func prerelease(version string) bool {
	lower := strings.ToLower(strings.SplitN(version, "+", 2)[0])
	for _, word := range []string{"alpha", "beta", "preview", "rc", "nightly", "dev", "snapshot"} {
		if strings.Contains(lower, word) {
			return true
		}
	}
	return false
}

var versionTokens = regexp.MustCompile(`[0-9]+|[^0-9]+`)
var versionFields = regexp.MustCompile(`[.\-_+]`)

func compareVersionNumber(a, b string) int {
	a = strings.TrimLeft(a, "0")
	b = strings.TrimLeft(b, "0")
	if len(a) < len(b) {
		return -1
	}
	if len(a) > len(b) {
		return 1
	}
	return strings.Compare(a, b)
}
func newerVersion(a, b string) bool {
	base := func(value string) []string {
		value = strings.ToLower(strings.SplitN(canonicalVersion(value), "+", 2)[0])
		end := len(value)
		for _, marker := range []string{"alpha", "beta", "preview", "rc", "nightly", "dev", "snapshot"} {
			if index := strings.Index(value, marker); index >= 0 && index < end {
				end = index
			}
		}
		parts := versionFields.Split(value[:end], -1)
		for i, part := range parts {
			n := 0
			for n < len(part) && part[n] >= '0' && part[n] <= '9' {
				n++
			}
			parts[i] = part[:n]
		}
		return parts
	}
	aa, bb := base(a), base(b)
	for i := 0; i < max(len(aa), len(bb)); i++ {
		x, y := "", ""
		if i < len(aa) {
			x = aa[i]
		}
		if i < len(bb) {
			y = bb[i]
		}
		if cmp := compareVersionNumber(x, y); cmp != 0 {
			return cmp > 0
		}
	}
	if prerelease(a) != prerelease(b) {
		return !prerelease(a)
	}
	aa = versionTokens.FindAllString(strings.ToLower(canonicalVersion(a)), -1)
	bb = versionTokens.FindAllString(strings.ToLower(canonicalVersion(b)), -1)
	for i := 0; i < min(len(aa), len(bb)); i++ {
		x, y := aa[i], bb[i]
		cmp := strings.Compare(x, y)
		if x[0] >= '0' && x[0] <= '9' && y[0] >= '0' && y[0] <= '9' {
			cmp = compareVersionNumber(x, y)
		}
		if cmp != 0 {
			return cmp > 0
		}
	}
	return len(aa) > len(bb)
}
func (c *Core) resolveInstallPackage(ctx context.Context, key string) (map[string]any, error) {
	if entry, err := c.findPackage(key); err == nil {
		return entry, nil
	}
	id, version, exact := strings.Cut(key, "@")
	if !exact || !safePart(id) || !safePart(version) {
		return nil, invalid("套件标识无效")
	}
	template, err := c.versionTemplate(id)
	if err != nil {
		return nil, err
	}
	cat, err := c.versionCatalog(ctx, id, false, false)
	if err != nil {
		return nil, err
	}
	for _, remote := range cat.Remote {
		if canonicalVersion(remote.Version) != canonicalVersion(version) {
			continue
		}
		data, _ := json.Marshal(template)
		entry := map[string]any{}
		if err = json.Unmarshal(data, &entry); err != nil {
			return nil, err
		}
		entry["version"] = remote.Version
		entry["url"] = remote.URL
		entry["sha256"] = remote.SHA256
		entry["sizeBytes"] = remote.SizeBytes
		entry["entry"] = remote.Entry
		entry["kind"] = remote.Kind
		entry["note"] = remote.Note
		entry["mirrors"] = []string{}
		return entry, nil
	}
	return nil, &AppError{Code: "PACKAGE_NOT_FOUND", Message: "版本目录中没有此可安装版本", Detail: cat.Error}
}
func (c *Core) registerVersions() {
	c.versions.gates = map[string]chan struct{}{}
	c.versions.slots = make(chan struct{}, 6)
	c.handle("version_catalog", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID         string `json:"id"`
			Force      bool   `json:"force"`
			CachedOnly bool   `json:"cachedOnly"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.versionCatalog(ctx, in.ID, in.Force, in.CachedOnly)
	})
	c.handle("version_catalogs", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Force bool `json:"force"`
		}](data)
		if err != nil {
			return nil, err
		}
		ids := []string{}
		seen := map[string]bool{}
		for _, template := range c.catalog.Packages {
			id, _ := template["id"].(string)
			source, err := sourceFor(template)
			if err != nil {
				return nil, err
			}
			if source.Kind != "" && !seen[id] {
				seen[id] = true
				ids = append(ids, id)
			}
		}
		sort.Strings(ids)
		out := make([]versionCatalog, len(ids))
		jobs := make(chan int)
		var wg sync.WaitGroup
		for worker := 0; worker < min(6, len(ids)); worker++ {
			wg.Add(1)
			go func() {
				defer wg.Done()
				for i := range jobs {
					cat, err := c.versionCatalog(ctx, ids[i], in.Force, false)
					if err != nil {
						cat = versionCatalog{ID: ids[i], Remote: []remoteVersion{}, Error: publicError(err).Message}
					}
					out[i] = cat
				}
			}()
		}
		for i := range ids {
			jobs <- i
		}
		close(jobs)
		wg.Wait()
		return out, nil
	})
}
func (c *Core) fetchVersions(ctx context.Context, template map[string]any, source versionSource) ([]remoteVersion, error) {
	switch source.Kind {
	case "github":
		return fetchGitHubVersions(ctx, template, source)
	case "nodejs":
		return c.fetchNodeVersions(ctx, template, source)
	case "npm-registry":
		return fetchNPMVersions(ctx, template, source)
	case "go":
		return fetchGoVersions(ctx, template, source)
	case "nginx", "php":
		return fetchWebVersions(ctx, template, source)
	default:
		return fetchOtherVersions(ctx, template, source)
	}
}
