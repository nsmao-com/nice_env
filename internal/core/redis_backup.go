package core

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"niceenv/internal/process"
)

type redisBackup struct {
	ID        string `json:"id"`
	Version   string `json:"version"`
	CreatedAt int64  `json:"createdAt"`
	SizeBytes int64  `json:"sizeBytes"`
	SHA256    string `json:"sha256"`
	Kind      string `json:"kind"`
}
type rdbDigest struct {
	Size   int64
	Hash   string
	Prefix []byte
}

var redisCRCTable = func() [256]uint64 {
	var table [256]uint64
	for i := range table {
		n := uint64(i)
		for j := 0; j < 8; j++ {
			if n&1 != 0 {
				n = n>>1 ^ 0x95ac9329ac4bc9b5
			} else {
				n >>= 1
			}
		}
		table[i] = n
	}
	return table
}()

func copyRDB(ctx context.Context, path string, out io.Writer, validate bool) (rdbDigest, error) {
	result := rdbDigest{}
	before, err := os.Lstat(path)
	if err != nil {
		return result, err
	}
	if !before.Mode().IsRegular() {
		return result, invalid("只支持无链接的普通文件")
	}
	f, err := os.Open(path)
	if err != nil {
		return result, err
	}
	defer f.Close()
	opened, err := f.Stat()
	if err != nil {
		return result, err
	}
	if !os.SameFile(before, opened) {
		return result, invalid("文件在打开时被替换")
	}
	if validate && before.Size() < 18 {
		return result, invalid("RDB 文件为空或过短")
	}
	hash := sha256.New()
	var crc uint64
	trailer := []byte{}
	var last byte
	buffer := make([]byte, 128*1024)
	for {
		if err = ctx.Err(); err != nil {
			return result, err
		}
		n, readErr := f.Read(buffer)
		if n > 0 {
			chunk := buffer[:n]
			if result.Size+int64(n) > before.Size() {
				return result, invalid("文件在复制时增长")
			}
			hash.Write(chunk)
			if len(result.Prefix) < 65536 {
				count := min(n, 65536-len(result.Prefix))
				result.Prefix = append(result.Prefix, chunk[:count]...)
			}
			if validate {
				payload := min(int64(n), max(int64(0), before.Size()-8-result.Size))
				for _, b := range chunk[:payload] {
					crc = redisCRCTable[byte(crc)^b] ^ (crc >> 8)
				}
				if payload > 0 {
					last = chunk[payload-1]
				}
				trailer = append(trailer, chunk[payload:]...)
			}
			if _, err = out.Write(chunk); err != nil {
				return result, err
			}
			result.Size += int64(n)
		}
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			return result, readErr
		}
	}
	after, err := f.Stat()
	if err != nil {
		return result, err
	}
	if result.Size != before.Size() || after.Size() != before.Size() || !after.ModTime().Equal(before.ModTime()) {
		return result, invalid("文件在复制时发生变化")
	}
	if validate {
		if len(result.Prefix) < 9 || string(result.Prefix[:5]) != "REDIS" {
			return result, invalid("文件不是 RDB 快照")
		}
		if _, err = strconv.ParseUint(string(result.Prefix[5:9]), 10, 16); err != nil {
			return result, invalid("RDB 格式版本无效")
		}
		if len(trailer) != 8 || last != 0xff {
			return result, invalid("RDB 结束标记不完整")
		}
		expected := binary.LittleEndian.Uint64(trailer)
		if expected != 0 && expected != crc {
			return result, invalid("RDB CRC64 校验失败")
		}
	}
	result.Hash = hex.EncodeToString(hash.Sum(nil))
	return result, nil
}
func rdbSourceVersion(prefix []byte) (int, string, error) {
	unknown := func() (int, string, error) {
		return 0, "", invalid("RDB 缺少可识别的 redis-ver 元信息，无法自动导入")
	}
	if len(prefix) < 9 {
		return unknown()
	}
	format, err := strconv.Atoi(string(prefix[5:9]))
	if err != nil {
		return unknown()
	}
	r := bytes.NewReader(prefix[9:])
	readString := func() (string, error) {
		first, err := r.ReadByte()
		if err != nil {
			return "", err
		}
		var n uint64
		switch first >> 6 {
		case 0:
			n = uint64(first)
		case 1:
			next, err := r.ReadByte()
			if err != nil {
				return "", err
			}
			n = uint64(first&63)<<8 | uint64(next)
		case 2:
			if first == 0x80 {
				var v uint32
				err = binary.Read(r, binary.BigEndian, &v)
				n = uint64(v)
			} else if first == 0x81 {
				err = binary.Read(r, binary.BigEndian, &n)
			} else {
				return "", invalid("RDB 长度编码不支持")
			}
			if err != nil {
				return "", err
			}
		case 3:
			switch first & 63 {
			case 0:
				v, err := r.ReadByte()
				return strconv.FormatInt(int64(int8(v)), 10), err
			case 1:
				var v int16
				err := binary.Read(r, binary.LittleEndian, &v)
				return strconv.FormatInt(int64(v), 10), err
			case 2:
				var v int32
				err := binary.Read(r, binary.LittleEndian, &v)
				return strconv.FormatInt(int64(v), 10), err
			default:
				return "", invalid("RDB 压缩元数据无法自动识别")
			}
		}
		if n > 4096 || n > uint64(r.Len()) {
			return "", invalid("RDB 元信息长度无效")
		}
		data := make([]byte, n)
		_, err = io.ReadFull(r, data)
		return string(data), err
	}
	for i := 0; i < 64; i++ {
		opcode, err := r.ReadByte()
		if err != nil || opcode != 0xfa {
			break
		}
		key, err := readString()
		if err != nil {
			return unknown()
		}
		value, err := readString()
		if err != nil {
			return unknown()
		}
		if key == "redis-ver" {
			parts := strings.Split(value, ".")
			if len(parts) < 3 || len(parts) > 4 {
				return unknown()
			}
			for _, part := range parts {
				if len(part) == 0 || len(part) > 5 {
					return unknown()
				}
				if _, err = strconv.ParseUint(part, 10, 32); err != nil {
					return unknown()
				}
			}
			return format, value, nil
		}
	}
	return unknown()
}
func (c *Core) redisBackupDir() (string, error) {
	dir := filepath.Join(c.Home, "backup", "redis")
	return dir, managedDirectory(c.Home, dir)
}
func (c *Core) redisBackupPath(id string) (string, error) {
	if !safePart(id) || len(id) > 100 {
		return "", invalid("备份标识无效")
	}
	dir, err := c.redisBackupDir()
	if err != nil {
		return "", err
	}
	path := filepath.Join(dir, id)
	info, err := os.Lstat(path)
	if err != nil {
		return "", err
	}
	if !info.IsDir() || info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
		return "", invalid("备份路径不是普通目录")
	}
	return path, nil
}
func (c *Core) redisBackupMetadata(id string) (redisBackup, string, error) {
	var item redisBackup
	dir, err := c.redisBackupPath(id)
	if err != nil {
		return item, "", err
	}
	text, err := readConfigText(filepath.Join(dir, "metadata.json"))
	if err != nil {
		return item, "", err
	}
	if len(text) > 8192 {
		return item, "", invalid("备份元数据过大")
	}
	if err = json.Unmarshal([]byte(text), &item); err != nil {
		return item, "", invalid("备份元数据损坏")
	}
	if item.ID != id || item.CreatedAt <= 0 || !safePart(item.Version) || item.SizeBytes < 18 || len(item.SHA256) != 64 || !slices.Contains([]string{"snapshot", "before-restore", "imported"}, item.Kind) {
		return item, "", invalid("备份元数据字段无效")
	}
	return item, dir, nil
}
func (c *Core) verifyRedisBackup(ctx context.Context, id string) (redisBackup, string, error) {
	item, dir, err := c.redisBackupMetadata(id)
	if err != nil {
		return item, dir, err
	}
	digest, err := copyRDB(ctx, filepath.Join(dir, "content.rdb"), io.Discard, true)
	if err != nil {
		return item, dir, err
	}
	if digest.Hash != item.SHA256 || digest.Size != item.SizeBytes {
		return item, dir, invalid("备份大小或摘要不符")
	}
	return item, dir, nil
}
func (c *Core) archiveRedis(ctx context.Context, version, source, kind, expected string) (redisBackup, error) {
	item := redisBackup{}
	dir, err := c.redisBackupDir()
	if err != nil {
		return item, err
	}
	id, err := newID("rdb")
	if err != nil {
		return item, err
	}
	stage, err := os.MkdirTemp(dir, ".rdb-pending-")
	if err != nil {
		return item, err
	}
	// A failed stage is retained for recovery; published entries are immutable.
	f, err := os.OpenFile(filepath.Join(stage, "content.rdb"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return item, err
	}
	digest, err := copyRDB(ctx, source, f, true)
	if err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err != nil {
		return item, err
	}
	if closeErr != nil {
		return item, closeErr
	}
	if expected != "" && expected != digest.Hash {
		return item, invalid("源文件与预览不同，未发布备份")
	}
	item = redisBackup{ID: id, Version: version, CreatedAt: time.Now().UnixMilli(), SizeBytes: digest.Size, SHA256: digest.Hash, Kind: kind}
	data, err := json.Marshal(item)
	if err != nil {
		return item, err
	}
	if err = atomicWrite(filepath.Join(stage, "metadata.json"), data); err != nil {
		return item, err
	}
	err = os.Rename(stage, filepath.Join(dir, id))
	return item, err
}
func (c *Core) redisBackupSummary(id string) map[string]any {
	result := map[string]any{"id": id, "version": nil, "createdAt": nil, "sizeBytes": nil, "kind": nil, "problem": nil}
	item, _, err := c.redisBackupMetadata(id)
	if err != nil {
		result["problem"] = "备份记录无法读取，请保留文件检查"
	} else {
		result["version"] = item.Version
		result["createdAt"] = item.CreatedAt
		result["sizeBytes"] = item.SizeBytes
		result["kind"] = item.Kind
	}
	dir, pathErr := c.redisBackupPath(id)
	if pathErr != nil {
		result["problem"] = "备份目录无法读取"
		return result
	}
	info, err := os.Lstat(filepath.Join(dir, "content.rdb"))
	if err != nil || !info.Mode().IsRegular() {
		result["problem"] = "RDB 文件缺失或不是普通文件"
	} else {
		if item.SizeBytes != 0 && item.SizeBytes != info.Size() {
			result["problem"] = "RDB 文件大小与记录不符"
		}
		result["sizeBytes"] = info.Size()
	}
	return result
}
func (c *Core) redisRemovalRevision(ctx context.Context, id string) (string, error) {
	dir, err := c.redisBackupPath(id)
	if err != nil {
		return "", err
	}
	return c.redisRemovalAt(ctx, id, dir)
}
func (c *Core) redisRemovalAt(ctx context.Context, id, dir string) (string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return "", err
	}
	scope := map[string]any{}
	for _, entry := range entries {
		if entry.Name() != "metadata.json" && entry.Name() != "content.rdb" {
			return "", invalid("备份目录包含额外文件，请手动检查")
		}
		digest, err := copyRDB(ctx, filepath.Join(dir, entry.Name()), io.Discard, false)
		if err != nil {
			return "", err
		}
		scope[entry.Name()] = []any{digest.Size, digest.Hash}
	}
	return siteRevision(c.Home, id, scope), nil
}
func (c *Core) redisImportPreview(ctx context.Context, source string) (map[string]any, error) {
	source = DisplayPath(source)
	if !filepath.IsAbs(source) {
		return nil, invalid("请选择绝对路径的 RDB 文件")
	}
	source = filepath.Clean(source)
	digest, err := copyRDB(ctx, source, io.Discard, true)
	if err != nil {
		return nil, err
	}
	format, version, err := rdbSourceVersion(digest.Prefix)
	if err != nil {
		return nil, err
	}
	revision := siteRevision(c.Home, source, []any{format, version, digest.Size, digest.Hash})
	return map[string]any{"source": Path(source), "version": version, "rdbVersion": format, "sizeBytes": digest.Size, "sha256": digest.Hash, "revision": revision}, nil
}

func (c *Core) redisRestoreTarget(ctx context.Context, version string) (string, string, error) {
	t, err := c.configTarget(ctx, "redis-conf@"+version)
	if err != nil {
		return "", "", err
	}
	text, err := readConfigText(t.path)
	if err != nil {
		return "", "", err
	}
	_, aof, err := parseRedisSettings(text)
	if err != nil {
		return "", "", err
	}
	if aof != nil && *aof {
		return "", "", invalid("此实例启用 AOF，不能只替换 RDB 恢复")
	}
	filename := "dump.rdb"
	for _, line := range strings.Split(text, "\n") {
		words := strings.Fields(line)
		if len(words) == 0 || strings.HasPrefix(words[0], "#") {
			continue
		}
		key := strings.ToLower(words[0])
		if key == "replicaof" || key == "slaveof" || key == "preload-file" && strings.TrimSpace(line[len(words[0]):]) != "\"\"" {
			return "", "", invalid("配置指定了复制或预载来源，不能保证使用恢复文件")
		}
		if key == "dbfilename" {
			value := strings.TrimSpace(strings.TrimSpace(line)[len(words[0]):])
			if len(value) >= 2 && (value[0] == '\'' || value[0] == '"') && value[len(value)-1] == value[0] {
				value = value[1 : len(value)-1]
			}
			if !safePart(value) || strings.ContainsAny(value, "/\\:") {
				return "", "", invalid("dbfilename 必须为单个普通文件名")
			}
			filename = value
		}
	}
	dir := filepath.Join(c.Home, "data", "redis")
	if err = managedDirectory(c.Home, dir); err != nil {
		return "", "", err
	}
	return filepath.Join(dir, filename), text, nil
}
func (c *Core) redisRestorePreview(ctx context.Context, version, id string) (map[string]any, error) {
	item, _, err := c.verifyRedisBackup(ctx, id)
	if err != nil {
		return nil, err
	}
	if canonicalVersion(item.Version) != canonicalVersion(version) {
		e, err := c.selectedRedis(version)
		if err != nil {
			return nil, err
		}
		r, err := c.resolveService(ctx, e)
		if err != nil {
			return nil, err
		}
		out := process.NewLimitedBuffer(8192)
		probeCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
		defer cancel()
		child, err := process.Start(probeCtx, process.Spec{Program: r.program, Args: []string{"--version"}, Directory: r.cwd, Stdout: out, Stderr: out})
		if err != nil {
			return nil, err
		}
		result := child.Wait()
		matched := false
		for _, word := range strings.Fields(out.String()) {
			if strings.HasPrefix(word, "v=") && canonicalVersion(strings.TrimPrefix(word, "v=")) == canonicalVersion(item.Version) {
				matched = true
			}
		}
		if result.Err != nil || result.Code != 0 || !matched {
			return nil, invalid("请选择生成此备份的准确 Redis 版本")
		}
	}
	target, config, err := c.redisRestoreTarget(ctx, version)
	if err != nil {
		return nil, err
	}
	var size any
	var old any
	if _, err = os.Lstat(target); err == nil {
		digest, err := copyRDB(ctx, target, io.Discard, false)
		if err != nil {
			return nil, err
		}
		size = digest.Size
		old = []any{digest.Size, digest.Hash}
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	return map[string]any{"backup": item, "target": Path(target), "existingSize": size, "revision": siteRevision(c.Home, target, []any{item, config, old})}, nil
}

func (c *Core) registerRedisBackups() {
	c.handle("redis_backup_list", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "list")
	})
	c.handle("redis_backup_create", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "create")
	})
	c.handle("redis_backup_inspect_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "inspect")
	})
	c.handle("redis_backup_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "import")
	})
	c.handle("redis_backup_export", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "export")
	})
	c.handle("redis_backup_removal_preview", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "removal")
	})
	c.handle("redis_backup_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "delete")
	})
	c.handle("redis_restore_preview", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "preview")
	})
	c.handle("redis_backup_restore", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisBackupCommand(ctx, data, "restore")
	})
}
func (c *Core) redisBackupCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Version      string `json:"version"`
		ID           string `json:"id"`
		Source       string `json:"source"`
		Destination  string `json:"destination"`
		Revision     string `json:"revision"`
		Confirmation string `json:"confirmation"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	dir, err := c.redisBackupDir()
	if err != nil {
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(dir, ".backup.lock"))
	if err != nil {
		return nil, err
	}
	defer unlock()
	switch action {
	case "list":
		entries, err := os.ReadDir(dir)
		if err != nil {
			return nil, err
		}
		items := []map[string]any{}
		unreadable := 0
		for _, entry := range entries {
			if strings.HasPrefix(entry.Name(), ".") {
				continue
			}
			if !entry.IsDir() || !safePart(entry.Name()) {
				unreadable++
				continue
			}
			items = append(items, c.redisBackupSummary(entry.Name()))
		}
		return map[string]any{"items": items, "unreadable": unreadable, "directory": Path(dir)}, nil
	case "inspect":
		return c.redisImportPreview(ctx, in.Source)
	case "import":
		preview, err := c.redisImportPreview(ctx, in.Source)
		if err != nil {
			return nil, err
		}
		if in.Revision == "" || preview["revision"] != in.Revision {
			return nil, invalid("源 RDB 与预览不同，请重新检查")
		}
		return c.archiveRedis(ctx, preview["version"].(string), string(preview["source"].(Path)), "imported", preview["sha256"].(string))
	case "removal", "delete":
		revision, err := c.redisRemovalRevision(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		if action == "removal" {
			return map[string]any{"entry": c.redisBackupSummary(in.ID), "revision": revision}, nil
		}
		if in.Revision == "" || in.Revision != revision {
			return nil, invalid("备份删除范围已变化，请重新预览")
		}
		itemDir, err := c.redisBackupPath(in.ID)
		if err != nil {
			return nil, err
		}
		// Rename the exact managed entry before removing its two known files. Unknown
		// additions make Remove fail and are retained in the quarantine directory.
		token, err := newID(".delete")
		if err != nil {
			return nil, err
		}
		stage := filepath.Join(dir, token)
		if err = os.Rename(itemDir, stage); err != nil {
			return nil, err
		}
		current, err := c.redisRemovalAt(ctx, in.ID, stage)
		if err != nil || current != in.Revision {
			return nil, &AppError{Code: "REDIS_BACKUP_CHANGED", Message: "备份在删除准备期间变化，已保留在隔离目录供检查", Detail: DisplayPath(stage)}
		}
		for _, name := range []string{"content.rdb", "metadata.json"} {
			if err = os.Remove(filepath.Join(stage, name)); err != nil && !os.IsNotExist(err) {
				return nil, err
			}
		}
		return nil, os.Remove(stage)
	case "export":
		item, itemDir, err := c.verifyRedisBackup(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		dest := DisplayPath(in.Destination)
		if !filepath.IsAbs(dest) {
			return nil, invalid("请选择导出路径")
		}
		parent := filepath.Dir(dest)
		root, err := os.OpenRoot(parent)
		if err != nil {
			return nil, err
		}
		defer root.Close()
		if _, err = root.Lstat(filepath.Base(dest)); !os.IsNotExist(err) {
			return nil, invalid("导出目标已存在或无法访问")
		}
		tmp, err := newID(".rdb-export")
		if err != nil {
			return nil, err
		}
		f, err := root.OpenFile(tmp, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		if err != nil {
			return nil, err
		}
		defer root.Remove(tmp)
		digest, err := copyRDB(ctx, filepath.Join(itemDir, "content.rdb"), f, true)
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
		if digest.Hash != item.SHA256 {
			return nil, invalid("导出期间备份变化")
		}
		if err = root.Link(tmp, filepath.Base(dest)); err != nil {
			return nil, err
		}
		return Path(dest), nil
	case "preview", "restore":
		e, err := c.selectedRedis(in.Version)
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		running := e.child != nil
		c.services.mu.RUnlock()
		if running {
			return nil, invalid("请先正常停止 Redis 再检查恢复范围")
		}
		lockDir := filepath.Join(c.Home, "service-locks")
		if err = managedDirectory(c.Home, lockDir); err != nil {
			return nil, err
		}
		serviceUnlock, err := process.Lock(filepath.Join(lockDir, e.status.ID+".lock"))
		if err != nil {
			return nil, &AppError{Code: "SERVICE_OWNED_ELSEWHERE", Message: "Redis 由其它实例持有，不能恢复数据"}
		}
		defer serviceUnlock()
		preview, err := c.redisRestorePreview(ctx, in.Version, in.ID)
		if err != nil {
			return nil, err
		}
		if action == "preview" {
			return preview, nil
		}
		if in.Confirmation != "Redis "+in.Version {
			return nil, invalid("请输入 Redis 名称及准确版本确认恢复全部逻辑库")
		}
		if in.Revision == "" || preview["revision"] != in.Revision {
			return nil, invalid("备份、配置或当前 RDB 已变化")
		}
		target := string(preview["target"].(Path))
		backup := preview["backup"].(redisBackup)
		_, itemDir, err := c.verifyRedisBackup(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		f, err := os.CreateTemp(filepath.Dir(target), ".rdb-restore-")
		if err != nil {
			return nil, err
		}
		defer os.Remove(f.Name())
		digest, err := copyRDB(ctx, filepath.Join(itemDir, "content.rdb"), f, true)
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
		if digest.Hash != backup.SHA256 {
			return nil, invalid("恢复期间备份变化")
		}
		var safety any
		if preview["existingSize"] != nil {
			item, err := c.archiveRedis(ctx, in.Version, target, "before-restore", "")
			if err != nil {
				return nil, err
			}
			safety = item
		}
		current, err := c.redisRestorePreview(ctx, in.Version, in.ID)
		if err != nil {
			return nil, err
		}
		if current["revision"] != in.Revision {
			return nil, invalid("恢复准备期间文件变化，未替换目标")
		}
		if err = os.Rename(f.Name(), target); err != nil {
			return nil, err
		}
		return map[string]any{"target": Path(target), "safetyBackup": safety}, nil
	case "create":
		return c.createRedisBackup(ctx, in.Version)
	}
	return nil, invalid("未知 Redis 备份操作")
}
func (c *Core) createRedisBackup(ctx context.Context, version string) (any, error) {
	e, err := c.selectedRedis(version)
	if err != nil {
		return nil, err
	}
	client, err := c.redisConnect(ctx, e, nil)
	if err != nil {
		return nil, err
	}
	defer client.close()
	before, err := redisPersistence(client, e.pkg.Version)
	if err != nil {
		return nil, err
	}
	if before["loading"] == true || before["saving"] == true || before["aofRewriting"] == true {
		return nil, invalid("Redis 正在加载或持久化，请稍后备份")
	}
	configGet := func(name string) (string, error) {
		value, err := client.command("CONFIG", "GET", name)
		if err != nil {
			return "", err
		}
		parts, ok := value.([]any)
		if !ok || len(parts) != 2 {
			return "", invalid("Redis 配置响应无效")
		}
		text, ok := parts[1].(string)
		if !ok {
			return "", invalid("Redis 配置响应无效")
		}
		return text, nil
	}
	actualDir, err := configGet("dir")
	if err != nil {
		return nil, err
	}
	if strings.HasPrefix(actualDir, "/cygdrive/") && len(actualDir) > 11 {
		actualDir = actualDir[10:11] + ":" + actualDir[11:]
	}
	actualDir = filepath.Clean(filepath.FromSlash(actualDir))
	expected := filepath.Join(c.Home, "data", "redis")
	if !strings.EqualFold(actualDir, expected) {
		return nil, invalid("Redis 实际持久化目录与受管目录不符")
	}
	filename, err := configGet("dbfilename")
	if err != nil {
		return nil, err
	}
	if !safePart(filename) || strings.ContainsAny(filename, "/\\:") {
		return nil, invalid("Redis 快照文件名无效")
	}
	source := filepath.Join(expected, filename)
	var oldStamp time.Time
	if info, err := os.Stat(source); err == nil {
		oldStamp = info.ModTime()
	}
	if _, err = client.command("BGSAVE"); err != nil {
		return nil, err
	}
	timer := time.NewTimer(90 * time.Second)
	defer timer.Stop()
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-timer.C:
			return nil, invalid("Redis 快照等待超时，未创建备份记录")
		case <-ticker.C:
		}
		info, err := redisPersistence(client, e.pkg.Version)
		if err != nil {
			return nil, err
		}
		if info["runId"] != before["runId"] {
			return nil, invalid("Redis 实例发生变化")
		}
		if info["saving"] == false {
			if info["lastSaveStatus"] != "ok" {
				return nil, invalid("Redis 快照失败，请检查日志")
			}
			stat, err := os.Stat(source)
			if err == nil && !stat.ModTime().Equal(oldStamp) {
				return c.archiveRedis(ctx, e.pkg.Version, source, "snapshot", "")
			}
		}
	}
}
