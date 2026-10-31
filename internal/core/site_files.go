package core

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"unicode"
	"unicode/utf8"

	"niceenv/internal/process"
)

const siteTextLimit = 1024 * 1024
const siteTransferLimit = 64 * 1024 * 1024

type siteFileRequest struct {
	ID               string `json:"id"`
	Current          string `json:"current"`
	Path             string `json:"path"`
	NewPath          string `json:"newPath"`
	Content          string `json:"content"`
	ExpectedRevision string `json:"expectedRevision"`
	Confirmation     string `json:"confirmation"`
	Directory        bool   `json:"directory"`
	Source           string `json:"source"`
	Destination      string `json:"destination"`
}
type siteFileEntry struct {
	Name       string `json:"name"`
	Path       string `json:"path"`
	Directory  bool   `json:"directory"`
	SizeBytes  int64  `json:"sizeBytes"`
	ModifiedAt int64  `json:"modifiedAt"`
}
type siteTextFile struct {
	SiteID    string `json:"siteId"`
	Path      string `json:"path"`
	SizeBytes int    `json:"sizeBytes"`
	Revision  string `json:"revision"`
	Content   string `json:"content"`
}

func (c *Core) registerSiteFiles() {
	c.handle("site_directory", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteFileCommand(ctx, data, "list")
	})
	c.handle("site_file_read", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteFileCommand(ctx, data, "read")
	})
	c.handle("site_file_write", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteFileCommand(ctx, data, "write")
	})
	c.handle("site_file_create", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteFileCommand(ctx, data, "create")
	})
	c.handle("site_file_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteFileCommand(ctx, data, "delete")
	})
	c.handle("site_file_rename", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteFileCommand(ctx, data, "rename")
	})
	c.handle("site_file_upload", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteFileCommand(ctx, data, "upload")
	})
	c.handle("site_file_download", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.siteFileCommand(ctx, data, "download")
	})
}

func siteRelative(value string) (string, error) {
	if len(value) > 2048 || strings.IndexFunc(value, unicode.IsControl) >= 0 || strings.Contains(value, ":") || strings.HasPrefix(value, "/") || strings.HasPrefix(value, "\\") {
		return "", invalid("文件路径必须位于站点根目录内")
	}
	parts := strings.Split(strings.ReplaceAll(value, "\\", "/"), "/")
	out := []string{}
	for _, part := range parts {
		if part == "." || part == "" {
			continue
		}
		if part == ".." || strings.HasSuffix(part, ".") || strings.HasSuffix(part, " ") {
			return "", invalid("文件路径包含不安全的目录名")
		}
		out = append(out, part)
	}
	if len(out) == 0 {
		return ".", nil
	}
	return filepath.Join(out...), nil
}
func siteSensitive(name string) bool {
	name = strings.ToLower(filepath.Base(name))
	if name == ".env" || strings.HasPrefix(name, ".env.") {
		return true
	}
	switch name {
	case "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519":
		return true
	}
	for _, suffix := range []string{".key", ".pem", ".p12", ".pfx", ".crt", ".cer"} {
		if strings.HasSuffix(name, suffix) {
			return true
		}
	}
	return false
}
func siteDisplay(name string) string {
	if name == "." {
		return ""
	}
	return filepath.ToSlash(name)
}

// Root operations enforce the directory boundary even if an external editor
// replaces a path between validation and I/O. Explicit checks also hide links.
func siteCheck(root *os.Root, name string, allowMissing bool) (os.FileInfo, error) {
	current := "."
	parts := strings.Split(name, string(filepath.Separator))
	for i, part := range parts {
		current = filepath.Join(current, part)
		info, err := root.Lstat(current)
		if os.IsNotExist(err) && allowMissing && i == len(parts)-1 {
			return nil, nil
		}
		if err != nil {
			return nil, err
		}
		if info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return nil, invalid("文件路径不能经过符号链接或目录联接")
		}
		if i < len(parts)-1 && !info.IsDir() {
			return nil, invalid("父目录不是普通目录")
		}
		if i == len(parts)-1 {
			return info, nil
		}
	}
	return nil, invalid("文件路径无效")
}
func (c *Core) siteRoot(ctx context.Context, id string) (*os.Root, error) {
	var path, runtimeText string
	err := c.Store.db.QueryRowContext(ctx, "SELECT root_dir,runtime FROM sites WHERE id=?", id).Scan(&path, &runtimeText)
	if err == sql.ErrNoRows {
		return nil, invalid("站点已不存在，请刷新列表")
	}
	if err != nil {
		return nil, err
	}
	var runtimeData struct {
		Kind string `json:"kind"`
	}
	if err = json.Unmarshal([]byte(runtimeText), &runtimeData); err != nil {
		return nil, err
	}
	path = DisplayPath(path)
	if path == "" || runtimeData.Kind == "redirect" {
		return nil, invalid("此站点没有可浏览的项目目录")
	}
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.IsDir() || info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
		return nil, invalid("站点根目录必须是普通目录")
	}
	actual, err := filepath.EvalSymlinks(path)
	if err != nil {
		return nil, err
	}
	return os.OpenRoot(actual)
}
func siteRevision(root, name string, value any) string {
	payload, _ := json.Marshal([]any{root, name, value})
	digest := sha256.Sum256(payload)
	return hex.EncodeToString(digest[:])
}
func siteReadText(root *os.Root, name string) ([]byte, error) {
	if name == "." || siteSensitive(name) {
		return nil, invalid("环境变量和密钥文件请使用专用编辑器")
	}
	info, err := siteCheck(root, name, false)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Size() > siteTextLimit {
		return nil, invalid("请选择不超过 1 MiB 的普通文本文件")
	}
	f, err := root.Open(name)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	value, err := io.ReadAll(io.LimitReader(f, siteTextLimit+1))
	if err != nil {
		return nil, err
	}
	if len(value) > siteTextLimit || bytes.IndexByte(value, 0) >= 0 || !utf8.Valid(value) {
		return nil, invalid("文件过大或不是 UTF-8 文本")
	}
	return value, nil
}
func siteTextResult(id, name string, root *os.Root, value []byte) siteTextFile {
	return siteTextFile{SiteID: id, Path: siteDisplay(name), SizeBytes: len(value), Revision: siteRevision(root.Name(), name, value), Content: string(value)}
}

func (c *Core) siteFileCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[siteFileRequest](data)
	if err != nil {
		return nil, err
	}
	if !c.siteChanges.TryLock() {
		return nil, &AppError{Code: "SITE_BUSY", Message: "站点文件正在操作，请稍后重试"}
	}
	defer c.siteChanges.Unlock()
	root, err := c.siteRoot(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	defer root.Close()
	lockDir := filepath.Join(c.Home, "site-locks")
	if err = managedDirectory(c.Home, lockDir); err != nil {
		return nil, err
	}
	digest := sha256.Sum256([]byte(root.Name()))
	unlock, err := process.Lock(filepath.Join(lockDir, hex.EncodeToString(digest[:])+".lock"))
	if err != nil {
		return nil, err
	}
	defer unlock()
	if action == "list" {
		in.Path = in.Current
	}
	name, err := siteRelative(in.Path)
	if err != nil {
		return nil, err
	}
	if action == "list" {
		info, err := siteCheck(root, name, false)
		if err != nil {
			return nil, err
		}
		if !info.IsDir() {
			return nil, invalid("请选择普通目录")
		}
		f, err := root.Open(name)
		if err != nil {
			return nil, err
		}
		entries, err := f.ReadDir(501)
		f.Close()
		if err != nil && err != io.EOF {
			return nil, err
		}
		if len(entries) > 500 {
			return nil, invalid("目录超过 500 项，请使用系统文件管理器")
		}
		items := []siteFileEntry{}
		for _, entry := range entries {
			path := filepath.Join(name, entry.Name())
			meta, err := root.Lstat(path)
			if err != nil {
				return nil, err
			}
			if meta.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 || (!meta.IsDir() && !meta.Mode().IsRegular()) {
				continue
			}
			size := meta.Size()
			if meta.IsDir() {
				size = 0
			}
			items = append(items, siteFileEntry{Name: entry.Name(), Path: siteDisplay(path), Directory: meta.IsDir(), SizeBytes: size, ModifiedAt: meta.ModTime().UnixMilli()})
		}
		sort.Slice(items, func(i, j int) bool {
			if items[i].Directory != items[j].Directory {
				return items[i].Directory
			}
			a, b := strings.ToLower(items[i].Name), strings.ToLower(items[j].Name)
			if a == b {
				return items[i].Name < items[j].Name
			}
			return a < b
		})
		var parent any
		if name != "." {
			parent = siteDisplay(filepath.Dir(name))
		}
		return map[string]any{"siteId": in.ID, "root": Path(root.Name()), "current": siteDisplay(name), "parent": parent, "revision": siteRevision(root.Name(), name, items), "entries": items}, nil
	}
	if name == "." || siteSensitive(name) {
		return nil, invalid("不能操作站点根目录；环境变量和密钥文件请使用专用入口")
	}
	receipt := map[string]any{"siteId": in.ID, "path": siteDisplay(name)}
	switch action {
	case "read", "write":
		previous, err := siteReadText(root, name)
		if err != nil {
			return nil, err
		}
		if action == "read" {
			return siteTextResult(in.ID, name, root, previous), nil
		}
		if in.ExpectedRevision == "" || siteRevision(root.Name(), name, previous) != in.ExpectedRevision {
			return nil, invalid("文件已变化，请重新打开后再保存")
		}
		if len(in.Content) > siteTextLimit || strings.ContainsRune(in.Content, 0) || !utf8.ValidString(in.Content) {
			return nil, invalid("内容过大或不是有效文本")
		}
		info, err := root.Stat(name)
		if err != nil {
			return nil, err
		}
		if info.Mode().Perm()&0200 == 0 {
			return nil, invalid("文件为只读")
		}
		tmp, err := newID(".niceenv-write")
		if err != nil {
			return nil, err
		}
		tmp = filepath.Join(filepath.Dir(name), tmp)
		f, err := root.OpenFile(tmp, os.O_CREATE|os.O_EXCL|os.O_WRONLY, info.Mode().Perm())
		if err != nil {
			return nil, err
		}
		defer root.Remove(tmp)
		_, err = f.WriteString(in.Content)
		if err == nil {
			err = f.Sync()
		}
		closeErr := f.Close()
		if err != nil {
			return nil, err
		}
		if closeErr != nil {
			return nil, closeErr
		}
		current, err := siteReadText(root, name)
		if err != nil {
			return nil, err
		}
		if !bytes.Equal(current, previous) {
			return nil, invalid("文件已被外部修改，请重新读取")
		}
		if err = ctx.Err(); err != nil {
			return nil, err
		}
		if err = root.Rename(tmp, name); err != nil {
			return nil, err
		}
		return siteTextResult(in.ID, name, root, []byte(in.Content)), nil
	case "create":
		info, err := siteCheck(root, name, true)
		if err != nil {
			return nil, err
		}
		if info != nil {
			return nil, invalid("同名文件或目录已存在")
		}
		if in.Directory {
			err = root.Mkdir(name, 0755)
		} else {
			var f *os.File
			f, err = root.OpenFile(name, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0644)
			if err == nil {
				err = f.Close()
			}
		}
		receipt["directory"] = in.Directory
		return receipt, err
	case "delete":
		if in.Confirmation != siteDisplay(name) {
			return nil, invalid("请输入完整相对路径确认删除")
		}
		info, err := siteCheck(root, name, false)
		if err != nil {
			return nil, err
		}
		if !info.IsDir() && !info.Mode().IsRegular() {
			return nil, invalid("只能删除普通文件或空目录")
		}
		receipt["directory"] = info.IsDir()
		return receipt, root.Remove(name)
	case "rename":
		target, err := siteRelative(in.NewPath)
		if err != nil {
			return nil, err
		}
		if target == "." || target == name || siteSensitive(target) {
			return nil, invalid("新名称无效或为敏感文件")
		}
		info, err := siteCheck(root, name, false)
		if err != nil {
			return nil, err
		}
		if !info.IsDir() && !info.Mode().IsRegular() {
			return nil, invalid("只能重命名普通文件或目录")
		}
		dest, err := siteCheck(root, target, true)
		if err != nil {
			return nil, err
		}
		if dest != nil {
			return nil, invalid("目标已存在")
		}
		if info.IsDir() && strings.HasPrefix(strings.ToLower(target), strings.ToLower(name)+string(filepath.Separator)) {
			return nil, invalid("目录不能移入自身的子目录")
		}
		receipt["from"] = siteDisplay(name)
		receipt["path"] = siteDisplay(target)
		receipt["directory"] = info.IsDir()
		return receipt, root.Rename(name, target)
	case "upload":
		info, err := siteCheck(root, name, true)
		if err != nil {
			return nil, err
		}
		if info != nil {
			return nil, invalid("目标文件已存在")
		}
		source := DisplayPath(in.Source)
		meta, err := os.Lstat(source)
		if err != nil {
			return nil, err
		}
		if !meta.Mode().IsRegular() || meta.Size() > siteTransferLimit {
			return nil, invalid("请选择不超过 64 MiB 的普通文件")
		}
		input, err := os.Open(source)
		if err != nil {
			return nil, err
		}
		defer input.Close()
		size, err := siteCopyNew(ctx, root, name, input)
		receipt["sizeBytes"] = size
		return receipt, err
	case "download":
		info, err := siteCheck(root, name, false)
		if err != nil {
			return nil, err
		}
		if !info.Mode().IsRegular() || info.Size() > siteTransferLimit {
			return nil, invalid("请选择不超过 64 MiB 的普通文件")
		}
		destination := DisplayPath(in.Destination)
		if !filepath.IsAbs(destination) || strings.IndexFunc(destination, unicode.IsControl) >= 0 || siteSensitive(destination) {
			return nil, invalid("请选择有效的导出文件路径")
		}
		parent := filepath.Dir(destination)
		meta, err := os.Lstat(parent)
		if err != nil {
			return nil, err
		}
		if !meta.IsDir() || meta.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return nil, invalid("导出父目录不能是链接")
		}
		destRoot, err := os.OpenRoot(parent)
		if err != nil {
			return nil, err
		}
		defer destRoot.Close()
		input, err := root.Open(name)
		if err != nil {
			return nil, err
		}
		defer input.Close()
		size, err := siteCopyNew(ctx, destRoot, filepath.Base(destination), input)
		receipt["destination"] = Path(destination)
		receipt["sizeBytes"] = size
		return receipt, err
	}
	return nil, invalid("未知站点文件操作")
}
func siteCopyNew(ctx context.Context, root *os.Root, name string, input io.Reader) (int64, error) {
	tmp, err := newID(".niceenv-copy")
	if err != nil {
		return 0, err
	}
	tmp = filepath.Join(filepath.Dir(name), tmp)
	f, err := root.OpenFile(tmp, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return 0, err
	}
	defer root.Remove(tmp)
	size, err := io.Copy(f, io.LimitReader(input, siteTransferLimit+1))
	if err == nil && size > siteTransferLimit {
		err = invalid("文件超过 64 MiB")
	}
	if err == nil {
		err = ctx.Err()
	}
	if err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err != nil {
		return 0, err
	}
	if closeErr != nil {
		return 0, closeErr
	}
	// Link publishes without replacing a concurrently created destination.
	if err = root.Link(tmp, name); err != nil {
		return 0, err
	}
	return size, nil
}
