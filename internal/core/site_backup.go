package core

import (
	"archive/zip"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"slices"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"niceenv/internal/process"
)

const siteArchiveFormat = "niceenv/site-files-v1"
const siteArchiveManifest = "niceenv-site-backup.json"
const siteArchiveEntries = 100000
const siteArchiveBytes int64 = 50 << 30
const siteArchiveLimit int64 = siteArchiveBytes + 1<<30
const siteManifestLimit = 32 << 20

var siteArchiveExcluded = []string{".git", "node_modules", ".next", ".nuxt", ".venv", "venv", "__pycache__", "target"}

type siteBackupScope struct {
	Root     Path     `json:"root"`
	Revision string   `json:"revision"`
	Excluded []string `json:"excluded"`
}
type siteBackupEntry struct {
	Path      string `json:"path"`
	Directory bool   `json:"directory"`
	Size      int64  `json:"size"`
	SHA256    string `json:"sha256"`
	Mode      uint32 `json:"mode"`
}
type siteBackupManifest struct {
	Format    string            `json:"format"`
	SiteID    string            `json:"siteId"`
	Root      Path              `json:"root"`
	CreatedAt int64             `json:"createdAt"`
	Excluded  []string          `json:"excluded"`
	Entries   []siteBackupEntry `json:"entries"`
	Automatic bool              `json:"automatic"`
}
type siteBackupInfo struct {
	Name          string   `json:"name"`
	Path          Path     `json:"path"`
	SizeBytes     int64    `json:"sizeBytes"`
	CreatedAt     int64    `json:"createdAt"`
	Files         int      `json:"files"`
	OriginalBytes int64    `json:"originalBytes"`
	Root          Path     `json:"root"`
	Excluded      []string `json:"excluded"`
	Restorable    bool     `json:"restorable"`
	Error         *string  `json:"error"`
	Automatic     bool     `json:"automatic"`
}
type siteBackupRequest struct {
	ID               string  `json:"id"`
	Name             string  `json:"name"`
	Source           string  `json:"source"`
	Parent           *string `json:"parent"`
	Project          bool    `json:"project"`
	ExcludeGenerated bool    `json:"excludeGenerated"`
	Revision         string  `json:"revision"`
	Confirmed        bool    `json:"confirmed"`
	Trusted          bool    `json:"trusted"`
	OperationID      string  `json:"operationId"`
}
type siteBackupProgress struct {
	c             *Core
	id, operation string
	last          time.Time
}

func (p *siteBackupProgress) send(phase string, files int, bytes int64) {
	if p.c.emit == nil || (phase != "complete" && time.Since(p.last) < 150*time.Millisecond) {
		return
	}
	p.last = time.Now()
	p.c.emit("site-files://progress", map[string]any{"operationId": p.operation, "siteId": p.id, "phase": phase, "files": files, "bytes": bytes})
}
func siteArchiveInvalid(message string) error {
	return &AppError{Code: "SITE_BACKUP_INVALID", Message: message}
}
func siteArchiveChanged() error {
	return &AppError{Code: "SITE_BACKUP_CHANGED", Message: "站点、目录或归档已变化，请重新检查并确认"}
}

func pathContains(parent, child string) bool {
	parent, child = filepath.Clean(DisplayPath(parent)), filepath.Clean(DisplayPath(child))
	if runtime.GOOS == "windows" {
		parent, child = strings.ToLower(parent), strings.ToLower(child)
	}
	rel, err := filepath.Rel(parent, child)
	return err == nil && (rel == "." || rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) && !filepath.IsAbs(rel))
}

// Open a stable directory handle, rejecting links in every ancestor. Root I/O
// keeps subsequent operations confined even if an outside editor renames paths.
func siteBackupRoot(path string) (*os.Root, error) {
	path = DisplayPath(path)
	if !filepath.IsAbs(path) {
		return nil, siteArchiveInvalid("请选择绝对目录路径")
	}
	path = filepath.Clean(path)
	for current := path; ; current = filepath.Dir(current) {
		info, err := os.Lstat(current)
		if err != nil {
			return nil, err
		}
		if !info.IsDir() || info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return nil, siteArchiveInvalid("目录不能经过符号链接、目录联接或特殊文件")
		}
		if filepath.Dir(current) == current {
			break
		}
	}
	return os.OpenRoot(path)
}

func siteBackupRootUnchanged(root *os.Root, expectedPath ...string) error {
	path := root.Name()
	if len(expectedPath) > 0 {
		path = expectedPath[0]
	}
	current, err := siteBackupRoot(path)
	if err != nil {
		return err
	}
	defer current.Close()
	before, err := root.Stat(".")
	if err != nil {
		return err
	}
	after, err := current.Stat(".")
	if err != nil {
		return err
	}
	if !os.SameFile(before, after) {
		return siteArchiveChanged()
	}
	return nil
}
func (c *Core) siteBackupScope(ctx context.Context, id string, project, exclude bool) (siteBackupScope, error) {
	var out siteBackupScope
	site, err := c.site(ctx, id)
	if err != nil {
		return out, err
	}
	if site.Runtime.Kind == "redirect" {
		return out, siteArchiveInvalid("跳转站点没有项目目录，请使用配置备份")
	}
	path := string(site.RootDir)
	if project {
		path = projectRoot(path)
		if app := site.Runtime.Application; app != nil && app.CWD != nil && strings.TrimSpace(string(*app.CWD)) != "" {
			path = string(*app.CWD)
		}
	}
	root, err := siteBackupRoot(path)
	if err != nil {
		return out, err
	}
	defer root.Close()
	out.Root = Path(root.Name())
	out.Excluded = []string{}
	if exclude {
		out.Excluded = append(out.Excluded, siteArchiveExcluded...)
	}
	out.Revision = siteRevision(id, "file-backup-scope", []any{site.RootDir, site.UpdatedAt, out.Root, project, out.Excluded})
	return out, nil
}
func (c *Core) siteBackupDirectory(id string) (string, error) {
	if id == "" || len(id) > 256 {
		return "", siteArchiveInvalid("站点标识无效")
	}
	digest := sha256.Sum256([]byte(id))
	path := filepath.Join(c.Home, "backup", "sites", hex.EncodeToString(digest[:]))
	if err := managedDirectory(c.Home, path); err != nil {
		return "", err
	}
	return path, nil
}
func (c *Core) siteBackupLock(id string) (func() error, error) {
	dir, err := c.siteBackupDirectory(id)
	if err != nil {
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(dir, ".files.lock"))
	if errors.Is(err, process.ErrLocked) {
		return nil, &AppError{Code: "SITE_BACKUP_BUSY", Message: "此站点正在备份、恢复或删除归档，请稍后再试"}
	}
	return unlock, err
}
func siteArchiveName(name string) bool {
	return len(name) <= 120 && strings.HasPrefix(name, "site-") && strings.HasSuffix(name, ".zip") && safePart(name)
}
func (c *Core) siteArchivePath(id, name string) (string, error) {
	if !siteArchiveName(name) {
		return "", siteArchiveInvalid("归档文件名无效")
	}
	dir, err := c.siteBackupDirectory(id)
	return filepath.Join(dir, name), err
}
func siteBackupOpen(root *os.Root, name string) (*os.File, error) {
	before, err := siteCheck(root, name, false)
	if err != nil {
		return nil, err
	}
	if !before.Mode().IsRegular() {
		return nil, siteArchiveInvalid("仅支持普通文件")
	}
	f, err := root.Open(name)
	if err != nil {
		return nil, err
	}
	after, err := f.Stat()
	if err != nil || !os.SameFile(before, after) || before.Size() != after.Size() || !before.ModTime().Equal(after.ModTime()) {
		f.Close()
		return nil, siteArchiveChanged()
	}
	return f, nil
}
func siteBackupRelative(name string) error {
	if name == "" || !utf8.ValidString(name) || strings.Contains(name, "\\") || strings.Count(name, "/") >= 128 {
		return siteArchiveInvalid("归档路径无效或层级过深")
	}
	value, err := siteRelative(name)
	if err != nil {
		return err
	}
	if filepath.ToSlash(value) != name {
		return siteArchiveInvalid("归档路径必须是规范相对路径")
	}
	for _, part := range strings.Split(name, "/") {
		base := strings.ToUpper(strings.SplitN(part, ".", 2)[0])
		device := false
		if strings.HasPrefix(base, "COM") || strings.HasPrefix(base, "LPT") {
			device = slices.Contains([]string{"1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"}, base[3:])
		}
		if strings.ContainsAny(part, `<>"|?*`) || base == "CON" || base == "PRN" || base == "AUX" || base == "NUL" || device {
			return siteArchiveInvalid("归档包含不可移植的文件名")
		}
	}
	return nil
}

type siteBackupSource struct {
	entry siteBackupEntry
	info  os.FileInfo
}

func collectSiteBackup(ctx context.Context, root *os.Root, excluded []string) ([]siteBackupSource, error) {
	out := []siteBackupSource{}
	total := int64(0)
	names := map[string]bool{}
	skip := map[string]bool{}
	for _, name := range excluded {
		skip[name] = true
	}
	var walk func(string) error
	walk = func(dir string) error {
		if err := ctx.Err(); err != nil {
			return err
		}
		f, err := root.Open(dir)
		if err != nil {
			return err
		}
		defer f.Close()
		for {
			items, readErr := f.ReadDir(256)
			for _, item := range items {
				name := item.Name()
				relative := name
				if dir != "." {
					relative = filepath.ToSlash(dir) + "/" + name
				}
				info, err := root.Lstat(filepath.FromSlash(relative))
				if err != nil {
					return err
				}
				if skip[name] && (info.IsDir() || info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0) {
					continue
				}
				if err = siteBackupRelative(relative); err != nil {
					return err
				}
				if !info.IsDir() && !info.Mode().IsRegular() || info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
					return siteArchiveInvalid("不支持链接或特殊文件：" + relative)
				}
				if names[strings.ToLower(relative)] {
					return siteArchiveInvalid("源目录包含大小写冲突")
				}
				names[strings.ToLower(relative)] = true
				size := info.Size()
				if info.IsDir() {
					size = 0
				}
				if size < 0 || size > siteArchiveBytes-total || len(out) >= siteArchiveEntries {
					return siteArchiveInvalid("单份归档最多支持 100,000 个条目和 50 GiB 文件")
				}
				total += size
				mode := info.Mode().Perm()
				if runtime.GOOS == "windows" {
					mode = 0644
					if info.IsDir() {
						mode = 0755
					} else if info.Mode().Perm()&0200 == 0 {
						mode = 0444
					}
				}
				out = append(out, siteBackupSource{siteBackupEntry{Path: relative, Directory: info.IsDir(), Size: size, Mode: uint32(mode)}, info})
				if info.IsDir() {
					if err = walk(filepath.FromSlash(relative)); err != nil {
						return err
					}
				}
			}
			if readErr == io.EOF {
				break
			}
			if readErr != nil {
				return readErr
			}
		}
		return nil
	}
	if err := walk("."); err != nil {
		return nil, err
	}
	sort.Slice(out, func(i, j int) bool { return out[i].entry.Path < out[j].entry.Path })
	return out, nil
}
func siteBackupInfoOf(path string, size int64, m siteBackupManifest) siteBackupInfo {
	out := siteBackupInfo{Name: filepath.Base(path), Path: Path(path), SizeBytes: size, CreatedAt: m.CreatedAt, Root: m.Root, Excluded: m.Excluded, Restorable: true, Automatic: m.Automatic}
	for _, e := range m.Entries {
		if !e.Directory {
			out.Files++
		}
		out.OriginalBytes += e.Size
	}
	return out
}
func siteBackupZipEntry(w *zip.Writer, e siteBackupEntry) (io.Writer, error) {
	h := &zip.FileHeader{Name: "files/" + e.Path, Method: zip.Deflate}
	mode := os.FileMode(e.Mode & 0777)
	if e.Directory {
		h.Name += "/"
		mode |= os.ModeDir
		h.Method = zip.Store
	}
	h.SetMode(mode)
	return w.CreateHeader(h)
}
func finishSiteArchive(file *os.File, w *zip.Writer, manifest siteBackupManifest) error {
	data, err := json.Marshal(manifest)
	if err != nil {
		return err
	}
	if len(data) > siteManifestLimit {
		return siteArchiveInvalid("归档清单过大")
	}
	member, err := w.Create(siteArchiveManifest)
	if err != nil {
		return err
	}
	if _, err = member.Write(data); err != nil {
		return err
	}
	if err = w.Close(); err != nil {
		return err
	}
	return file.Sync()
}
func publishSiteArchive(root *os.Root, tmp string, automatic bool) (string, error) {
	prefix := "site-"
	if automatic {
		prefix += "auto-"
	}
	name, err := newID(prefix + time.Now().Format("20060102-150405"))
	if err != nil {
		return "", err
	}
	name += ".zip"
	// Link is atomic and fails when the destination exists; Rename may replace it.
	if err = root.Link(tmp, name); err != nil {
		return "", err
	}
	return name, nil
}
func (c *Core) createSiteArchive(ctx context.Context, in siteBackupRequest, automatic bool, p *siteBackupProgress) (siteBackupInfo, error) {
	var empty siteBackupInfo
	scope, err := c.siteBackupScope(ctx, in.ID, in.Project, in.ExcludeGenerated)
	if err != nil {
		return empty, err
	}
	if scope.Revision != in.Revision {
		return empty, siteArchiveChanged()
	}
	dir, err := c.siteBackupDirectory(in.ID)
	if err != nil {
		return empty, err
	}
	if pathContains(dir, string(scope.Root)) || pathContains(string(scope.Root), dir) {
		return empty, siteArchiveInvalid("归档目录与源目录不能相互包含")
	}
	root, err := siteBackupRoot(string(scope.Root))
	if err != nil {
		return empty, err
	}
	defer root.Close()
	dest, err := siteBackupRoot(dir)
	if err != nil {
		return empty, err
	}
	defer dest.Close()
	p.send("scan", 0, 0)
	sources, err := collectSiteBackup(ctx, root, scope.Excluded)
	if err != nil {
		return empty, err
	}
	tmp, err := newID(".pending-")
	if err != nil {
		return empty, err
	}
	f, err := dest.OpenFile(tmp, os.O_CREATE|os.O_EXCL|os.O_RDWR, 0600)
	if err != nil {
		return empty, err
	}
	defer func() { f.Close(); dest.Remove(tmp) }()
	w := zip.NewWriter(f)
	manifest := siteBackupManifest{Format: siteArchiveFormat, SiteID: in.ID, Root: scope.Root, CreatedAt: time.Now().UnixMilli(), Excluded: scope.Excluded, Entries: []siteBackupEntry{}, Automatic: automatic}
	files := 0
	bytes := int64(0)
	buffer := make([]byte, 256<<10)
	for _, source := range sources {
		if err = ctx.Err(); err != nil {
			return empty, err
		}
		entry := source.entry
		out, err := siteBackupZipEntry(w, entry)
		if err != nil {
			return empty, err
		}
		if !entry.Directory {
			input, err := siteBackupOpen(root, filepath.FromSlash(entry.Path))
			if err != nil {
				return empty, err
			}
			digest := sha256.New()
			n, copyErr := io.CopyBuffer(io.MultiWriter(out, digest), io.LimitReader(contextReader{ctx, input}, entry.Size+1), buffer)
			after, statErr := input.Stat()
			closeErr := input.Close()
			if err = errors.Join(copyErr, statErr, closeErr); err != nil {
				return empty, err
			}
			if n != entry.Size || !os.SameFile(source.info, after) || !source.info.ModTime().Equal(after.ModTime()) {
				return empty, siteArchiveChanged()
			}
			entry.SHA256 = hex.EncodeToString(digest.Sum(nil))
			files++
			bytes += n
			p.send("backup", files, bytes)
		}
		manifest.Entries = append(manifest.Entries, entry)
	}
	current, err := collectSiteBackup(ctx, root, scope.Excluded)
	if err != nil {
		return empty, err
	}
	if len(current) != len(sources) {
		return empty, siteArchiveChanged()
	}
	for i, source := range sources {
		other := current[i]
		if !reflect.DeepEqual(source.entry, other.entry) || !os.SameFile(source.info, other.info) || !source.info.ModTime().Equal(other.info.ModTime()) {
			return empty, siteArchiveChanged()
		}
	}
	latest, err := c.siteBackupScope(ctx, in.ID, in.Project, in.ExcludeGenerated)
	if err != nil {
		return empty, err
	}
	if latest.Revision != in.Revision {
		return empty, siteArchiveChanged()
	}
	if err = siteBackupRootUnchanged(root); err != nil {
		return empty, err
	}
	if err = siteBackupRootUnchanged(dest); err != nil {
		return empty, err
	}
	if err = finishSiteArchive(f, w, manifest); err != nil {
		return empty, err
	}
	info, err := f.Stat()
	if err != nil {
		return empty, err
	}
	if err = f.Close(); err != nil {
		return empty, err
	}
	name, err := publishSiteArchive(dest, tmp, automatic)
	if err != nil {
		return empty, err
	}
	p.send("complete", files, bytes)
	return siteBackupInfoOf(filepath.Join(dir, name), info.Size(), manifest), nil
}

// Validate ZIP end records before archive/zip allocates its central directory.
// NiceEnv archives are single-disk, have no prefix and carry no ZIP comment.
func checkSiteArchive(f *os.File) (int, error) {
	info, err := f.Stat()
	if err != nil {
		return 0, err
	}
	length := info.Size()
	if length < 22 || length > siteArchiveLimit {
		return 0, siteArchiveInvalid("归档为空或超过 51 GiB")
	}
	end := make([]byte, 22)
	if _, err = f.ReadAt(end, length-22); err != nil {
		return 0, err
	}
	u16 := func(b []byte, i int) uint64 { return uint64(binary.LittleEndian.Uint16(b[i:])) }
	u32 := func(b []byte, i int) uint64 { return uint64(binary.LittleEndian.Uint32(b[i:])) }
	u64 := func(b []byte, i int) uint64 { return binary.LittleEndian.Uint64(b[i:]) }
	if string(end[:4]) != "PK\x05\x06" || u16(end, 20) != 0 || u16(end, 4) != 0 || u16(end, 6) != 0 || u16(end, 8) != u16(end, 10) {
		return 0, siteArchiveInvalid("请选择 NiceEnv 创建的完整单卷 ZIP")
	}
	count, size, offset, footer := u16(end, 10), u32(end, 12), u32(end, 16), uint64(length-22)
	if count == 65535 || size == 0xffffffff || offset == 0xffffffff {
		if length < 98 {
			return 0, siteArchiveInvalid("ZIP64 记录不完整")
		}
		locator := make([]byte, 20)
		if _, err = f.ReadAt(locator, length-42); err != nil {
			return 0, err
		}
		footer = u64(locator, 8)
		if string(locator[:4]) != "PK\x06\x07" || u32(locator, 4) != 0 || u32(locator, 16) != 1 || footer > uint64(length-98) || footer+56 != uint64(length-42) {
			return 0, siteArchiveInvalid("ZIP64 定位记录无效")
		}
		record := make([]byte, 56)
		if _, err = f.ReadAt(record, int64(footer)); err != nil {
			return 0, err
		}
		if string(record[:4]) != "PK\x06\x06" || u64(record, 4) != 44 || u32(record, 16) != 0 || u32(record, 20) != 0 || u64(record, 24) != u64(record, 32) {
			return 0, siteArchiveInvalid("ZIP64 结束记录无效")
		}
		count, size, offset = u64(record, 32), u64(record, 40), u64(record, 48)
	}
	if count == 0 || count > siteArchiveEntries+1 || size > 256<<20 || count*46 > size || offset > footer || size != footer-offset {
		return 0, siteArchiveInvalid("归档中央目录不完整或超限")
	}
	sig := make([]byte, 4)
	if _, err = f.ReadAt(sig, 0); err != nil {
		return 0, err
	}
	if string(sig) != "PK\x03\x04" {
		return 0, siteArchiveInvalid("不支持带前缀的 ZIP")
	}
	return int(count), nil
}
func readSiteArchive(f *os.File, id string) (*zip.Reader, siteBackupManifest, error) {
	var m siteBackupManifest
	count, err := checkSiteArchive(f)
	if err != nil {
		return nil, m, err
	}
	info, err := f.Stat()
	if err != nil {
		return nil, m, err
	}
	z, err := zip.NewReader(f, info.Size())
	if err != nil {
		return nil, m, err
	}
	if len(z.File) != count {
		return nil, m, siteArchiveInvalid("ZIP 条目数量不匹配")
	}
	members := map[string]*zip.File{}
	for _, member := range z.File {
		if members[member.Name] != nil {
			return nil, m, siteArchiveInvalid("ZIP 包含重复条目")
		}
		members[member.Name] = member
	}
	member := members[siteArchiveManifest]
	if member == nil || member.UncompressedSize64 > siteManifestLimit {
		return nil, m, siteArchiveInvalid("站点归档清单缺失或过大")
	}
	input, err := member.Open()
	if err != nil {
		return nil, m, err
	}
	data, err := io.ReadAll(io.LimitReader(input, siteManifestLimit+1))
	input.Close()
	if err != nil {
		return nil, m, err
	}
	if len(data) > siteManifestLimit || json.Unmarshal(data, &m) != nil {
		return nil, m, siteArchiveInvalid("站点归档清单无效")
	}
	if m.Format != siteArchiveFormat || m.SiteID == "" || len(m.SiteID) > 256 || id != "" && id != m.SiteID || len(m.Entries) > siteArchiveEntries || len(m.Entries)+1 != count {
		return nil, m, siteArchiveInvalid("归档不属于此站点或格式不受支持")
	}
	total := int64(0)
	seen := map[string]bool{}
	aliases := map[string]string{}
	for _, entry := range m.Entries {
		if err = siteBackupRelative(entry.Path); err != nil {
			return nil, m, err
		}
		key := strings.ToLower(entry.Path)
		if seen[key] {
			return nil, m, siteArchiveInvalid("归档路径重复")
		}
		seen[key] = true
		parts := strings.Split(entry.Path, "/")
		for n := 1; n <= len(parts); n++ {
			prefix := strings.Join(parts[:n], "/")
			value := prefix + "|file"
			if n < len(parts) || entry.Directory {
				value = prefix + "|dir"
			}
			alias := strings.ToLower(prefix)
			if old, ok := aliases[alias]; ok && old != value {
				return nil, m, siteArchiveInvalid("归档包含大小写或文件目录冲突")
			}
			aliases[alias] = value
		}
		if entry.Size < 0 || entry.Size > siteArchiveBytes-total || entry.Directory && entry.Size != 0 {
			return nil, m, siteArchiveInvalid("归档文件大小超限")
		}
		total += entry.Size
		name := "files/" + entry.Path
		if entry.Directory {
			name += "/"
		}
		file := members[name]
		if file == nil || file.UncompressedSize64 != uint64(entry.Size) || file.FileInfo().IsDir() != entry.Directory || !entry.Directory && !file.Mode().IsRegular() || entry.Directory && file.Mode().Type() != os.ModeDir {
			return nil, m, siteArchiveInvalid("归档文件类型、名称或大小不匹配")
		}
		if !entry.Directory {
			digest, e := hex.DecodeString(entry.SHA256)
			if e != nil || len(digest) != 32 {
				return nil, m, siteArchiveInvalid("归档缺少 SHA256 校验值")
			}
		}
	}
	return z, m, nil
}
func (c *Core) siteArchiveSnapshot(ctx context.Context, path string) (*os.File, string, func(), error) {
	root, err := siteBackupRoot(filepath.Dir(path))
	if err != nil {
		return nil, "", nil, err
	}
	defer root.Close()
	input, err := siteBackupOpen(root, filepath.Base(path))
	if err != nil {
		return nil, "", nil, err
	}
	defer input.Close()
	if _, err = checkSiteArchive(input); err != nil {
		return nil, "", nil, err
	}
	before, err := input.Stat()
	if err != nil {
		return nil, "", nil, err
	}
	dir := filepath.Join(c.Home, "tmp")
	if err = managedDirectory(c.Home, dir); err != nil {
		return nil, "", nil, err
	}
	snapshot, err := os.CreateTemp(dir, ".site-archive-")
	if err != nil {
		return nil, "", nil, err
	}
	cleanup := func() { snapshot.Close(); os.Remove(snapshot.Name()) }
	digest := sha256.New()
	n, err := io.CopyBuffer(io.MultiWriter(snapshot, digest), io.LimitReader(contextReader{ctx, input}, siteArchiveLimit+1), make([]byte, 256<<10))
	after, statErr := input.Stat()
	if e := errors.Join(err, statErr); e != nil {
		cleanup()
		return nil, "", nil, e
	}
	if n != before.Size() || n > siteArchiveLimit || after.Size() != before.Size() || !after.ModTime().Equal(before.ModTime()) {
		cleanup()
		return nil, "", nil, siteArchiveChanged()
	}
	return snapshot, hex.EncodeToString(digest.Sum(nil)), cleanup, nil
}
func verifySiteArchiveEntry(ctx context.Context, z *zip.Reader, entry siteBackupEntry, out io.Writer, buffer []byte) error {
	input, err := z.Open("files/" + entry.Path)
	if err != nil {
		return err
	}
	defer input.Close()
	digest := sha256.New()
	n, err := io.CopyBuffer(io.MultiWriter(out, digest), io.LimitReader(contextReader{ctx, input}, entry.Size+1), buffer)
	if err != nil {
		return err
	}
	if n != entry.Size || !strings.EqualFold(hex.EncodeToString(digest.Sum(nil)), entry.SHA256) {
		return siteArchiveInvalid("归档文件校验失败：" + entry.Path)
	}
	return nil
}
func (c *Core) listSiteArchives(ctx context.Context, id string) ([]siteBackupInfo, error) {
	dir, err := c.siteBackupDirectory(id)
	if err != nil {
		return nil, err
	}
	root, err := siteBackupRoot(dir)
	if err != nil {
		return nil, err
	}
	defer root.Close()
	f, err := root.Open(".")
	if err != nil {
		return nil, err
	}
	defer f.Close()
	out := []siteBackupInfo{}
	for {
		items, e := f.ReadDir(128)
		for _, item := range items {
			if err = ctx.Err(); err != nil {
				return nil, err
			}
			if !siteArchiveName(item.Name()) {
				continue
			}
			path := filepath.Join(dir, item.Name())
			info := siteBackupInfo{Name: item.Name(), Path: Path(path), Excluded: []string{}}
			input, readErr := siteBackupOpen(root, item.Name())
			if readErr == nil {
				stat, statErr := input.Stat()
				if statErr == nil {
					info.SizeBytes = stat.Size()
				}
				_, m, openErr := readSiteArchive(input, id)
				input.Close()
				readErr = errors.Join(statErr, openErr)
				if readErr == nil {
					info = siteBackupInfoOf(path, stat.Size(), m)
				}
			}
			if readErr != nil {
				message := publicError(readErr).Message
				info.Error = &message
			}
			out = append(out, info)
		}
		if e == io.EOF {
			break
		}
		if e != nil {
			return nil, e
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].CreatedAt == out[j].CreatedAt {
			return out[i].Name > out[j].Name
		}
		return out[i].CreatedAt > out[j].CreatedAt
	})
	return out, nil
}

func (c *Core) registerSiteBackups() {
	c.handle("site_files_scope", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[siteBackupRequest](data)
		if err != nil {
			return nil, err
		}
		return c.siteBackupScope(ctx, in.ID, in.Project, in.ExcludeGenerated)
	})
	c.handle("site_files_list", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[siteBackupRequest](data)
		if err != nil {
			return nil, err
		}
		if _, err = c.site(ctx, in.ID); err != nil {
			return nil, err
		}
		return c.listSiteArchives(ctx, in.ID)
	})
	c.handle("site_files_create", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteBackupCommand(ctx, data, "create")
	})
	c.handle("site_files_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteBackupCommand(ctx, data, "delete")
	})
	c.handle("site_files_inspect_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteBackupCommand(ctx, data, "inspectImport")
	})
	c.handle("site_files_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteBackupCommand(ctx, data, "import")
	})
	c.handle("site_files_inspect_restore", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteBackupCommand(ctx, data, "inspectRestore")
	})
	c.handle("site_files_restore", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteBackupCommand(ctx, data, "restore")
	})
}
func (c *Core) siteBackupCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[siteBackupRequest](data)
	if err != nil {
		return nil, err
	}
	if len(in.OperationID) > 200 {
		return nil, invalid("操作标识过长")
	}
	if (action == "create" || action == "import") && !in.Confirmed || action == "restore" && !in.Trusted {
		return nil, siteArchiveInvalid("请确认备份范围和归档来源；项目文件可能包含环境变量、密钥或可执行代码")
	}
	site, err := c.site(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	unlock, err := c.siteBackupLock(in.ID)
	if err != nil {
		return nil, err
	}
	defer unlock()
	p := &siteBackupProgress{c: c, id: in.ID, operation: in.OperationID}
	if action == "create" {
		return c.createSiteArchive(ctx, in, false, p)
	}
	path := in.Source
	if action == "delete" || action == "inspectRestore" || action == "restore" {
		path, err = c.siteArchivePath(in.ID, in.Name)
		if err != nil {
			return nil, err
		}
	}
	if action == "delete" {
		root, err := siteBackupRoot(filepath.Dir(path))
		if err != nil {
			return nil, err
		}
		defer root.Close()
		input, err := siteBackupOpen(root, in.Name)
		if err != nil {
			return nil, err
		}
		input.Close()
		return nil, root.Remove(in.Name)
	}
	p.send("inspect", 0, 0)
	snapshot, digest, cleanup, err := c.siteArchiveSnapshot(ctx, path)
	if err != nil {
		return nil, err
	}
	defer cleanup()
	expected := ""
	if action == "restore" || action == "inspectRestore" {
		expected = in.ID
	}
	z, m, err := readSiteArchive(snapshot, expected)
	if err != nil {
		return nil, err
	}
	stat, err := snapshot.Stat()
	if err != nil {
		return nil, err
	}
	info := siteBackupInfoOf(path, stat.Size(), m)
	buffer := make([]byte, 256<<10)
	revision := siteRevision(in.ID, "site-import", []any{digest, site.Name, site.RootDir, site.UpdatedAt})
	if action == "inspectImport" {
		for _, entry := range m.Entries {
			if !entry.Directory {
				if err = verifySiteArchiveEntry(ctx, z, entry, io.Discard, buffer); err != nil {
					return nil, err
				}
			}
		}
		current, err := c.site(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		if siteRevision(in.ID, "site-import", []any{digest, current.Name, current.RootDir, current.UpdatedAt}) != revision {
			return nil, siteArchiveChanged()
		}
		p.send("complete", info.Files, info.OriginalBytes)
		return map[string]any{"sourcePath": Path(path), "sourceSiteId": m.SiteID, "targetName": site.Name, "targetRoot": site.RootDir, "archive": info, "revision": revision}, nil
	}
	if action == "import" {
		if revision != in.Revision {
			return nil, siteArchiveChanged()
		}
		dir, err := c.siteBackupDirectory(in.ID)
		if err != nil {
			return nil, err
		}
		root, err := siteBackupRoot(dir)
		if err != nil {
			return nil, err
		}
		defer root.Close()
		tmp, err := newID(".pending-")
		if err != nil {
			return nil, err
		}
		f, err := root.OpenFile(tmp, os.O_CREATE|os.O_EXCL|os.O_RDWR, 0600)
		if err != nil {
			return nil, err
		}
		defer func() { f.Close(); root.Remove(tmp) }()
		w := zip.NewWriter(f)
		files := 0
		bytes := int64(0)
		for _, entry := range m.Entries {
			out, err := siteBackupZipEntry(w, entry)
			if err != nil {
				return nil, err
			}
			if !entry.Directory {
				if err = verifySiteArchiveEntry(ctx, z, entry, out, buffer); err != nil {
					return nil, err
				}
				files++
				bytes += entry.Size
				p.send("import", files, bytes)
			}
		}
		m.SiteID = in.ID
		m.Automatic = false
		if err = finishSiteArchive(f, w, m); err != nil {
			return nil, err
		}
		current, err := c.site(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		if siteRevision(in.ID, "site-import", []any{digest, current.Name, current.RootDir, current.UpdatedAt}) != in.Revision {
			return nil, siteArchiveChanged()
		}
		stat, err = f.Stat()
		if err != nil {
			return nil, err
		}
		if err = f.Close(); err != nil {
			return nil, err
		}
		if err = siteBackupRootUnchanged(root); err != nil {
			return nil, err
		}
		name, err := publishSiteArchive(root, tmp, false)
		if err != nil {
			return nil, err
		}
		p.send("complete", files, bytes)
		return siteBackupInfoOf(filepath.Join(dir, name), stat.Size(), m), nil
	}
	parent, err := c.siteRestoreParent(ctx, in.Parent)
	if err != nil {
		return nil, err
	}
	defer parent.Close()
	revision = siteRevision(revision, "site-restore", []any{in.Name, parent.Name()})
	if action == "restore" && revision != in.Revision {
		return nil, siteArchiveChanged()
	}
	name, err := newID("restored-site")
	if err != nil {
		return nil, err
	}
	if err = parent.Mkdir(name, 0700); err != nil {
		return nil, err
	}
	keep := false
	defer func() {
		if !keep {
			parent.RemoveAll(name)
		}
	}()
	output, err := parent.OpenRoot(name)
	if err != nil {
		return nil, err
	}
	defer output.Close()
	files := 0
	bytes := int64(0)
	for _, entry := range m.Entries {
		if err = ctx.Err(); err != nil {
			return nil, err
		}
		relative := filepath.FromSlash(entry.Path)
		if action == "inspectRestore" {
			if !entry.Directory {
				if err = verifySiteArchiveEntry(ctx, z, entry, io.Discard, buffer); err != nil {
					return nil, err
				}
			}
		} else if entry.Directory {
			if err = output.MkdirAll(relative, 0755); err != nil {
				return nil, err
			}
		} else {
			if err = output.MkdirAll(filepath.Dir(relative), 0755); err != nil {
				return nil, err
			}
			f, err := output.OpenFile(relative, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
			if err != nil {
				return nil, err
			}
			copyErr := verifySiteArchiveEntry(ctx, z, entry, f, buffer)
			syncErr := f.Sync()
			closeErr := f.Close()
			if err = errors.Join(copyErr, syncErr, closeErr); err != nil {
				return nil, err
			}
			if runtime.GOOS != "windows" {
				if err = output.Chmod(relative, os.FileMode(entry.Mode&0777)); err != nil {
					return nil, err
				}
			}
		}
		if !entry.Directory {
			files++
			bytes += entry.Size
		}
		phase := "restore"
		if action == "inspectRestore" {
			phase = "verify"
		}
		p.send(phase, files, bytes)
	}
	current, err := c.site(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	check, err := c.siteRestoreParent(ctx, in.Parent)
	if err != nil {
		return nil, err
	}
	check.Close()
	if err = siteBackupRootUnchanged(parent); err != nil {
		return nil, err
	}
	if err = siteBackupRootUnchanged(output, filepath.Join(parent.Name(), name)); err != nil {
		return nil, err
	}
	nowRevision := siteRevision(in.ID, "site-import", []any{digest, current.Name, current.RootDir, current.UpdatedAt})
	if siteRevision(nowRevision, "site-restore", []any{in.Name, check.Name()}) != revision {
		return nil, siteArchiveChanged()
	}
	p.send("complete", files, bytes)
	if action == "inspectRestore" {
		entries := []map[string]any{}
		for _, entry := range m.Entries[:min(100, len(m.Entries))] {
			entries = append(entries, map[string]any{"path": entry.Path, "directory": entry.Directory, "size": entry.Size})
		}
		return map[string]any{"archive": info, "parent": Path(parent.Name()), "revision": revision, "sha256": digest, "verifiedAt": time.Now().UnixMilli(), "entries": entries, "totalEntries": len(m.Entries)}, nil
	}
	keep = true
	return Path(filepath.Join(parent.Name(), name)), nil
}
func (c *Core) siteRestoreParent(ctx context.Context, value *string) (*os.Root, error) {
	path := filepath.Join(c.Home, "restored-sites")
	if value != nil && *value != "" {
		path = *value
	} else if err := managedDirectory(c.Home, path); err != nil {
		return nil, err
	}
	root, err := siteBackupRoot(path)
	if err != nil {
		return nil, err
	}
	reject := func() (*os.Root, error) {
		root.Close()
		return nil, siteArchiveInvalid("恢复目录不能位于已有站点项目或归档目录内")
	}
	if pathContains(filepath.Join(c.Home, "backup", "sites"), root.Name()) {
		return reject()
	}
	sites, err := c.sites(ctx)
	if err != nil {
		root.Close()
		return nil, err
	}
	for _, site := range sites {
		for _, project := range []bool{false, true} {
			scope, e := c.siteBackupScope(ctx, site.ID, project, false)
			if e == nil && pathContains(string(scope.Root), root.Name()) {
				return reject()
			}
		}
	}
	return root, nil
}
