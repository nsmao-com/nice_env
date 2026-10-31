package core

import (
	"compress/gzip"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"time"
	"unicode"

	"niceenv/internal/process"
)

type mongoBackup struct {
	ID           string `json:"id"`
	Database     string `json:"database"`
	Version      string `json:"version"`
	ToolsVersion string `json:"toolsVersion"`
	CreatedAt    int64  `json:"createdAt"`
	SizeBytes    int64  `json:"sizeBytes"`
	SHA256       string `json:"sha256"`
	Kind         string `json:"kind"`
}
type mongoBackupRequest struct {
	Version      string `json:"version"`
	Database     string `json:"database"`
	ID           string `json:"id"`
	Target       string `json:"target"`
	Source       string `json:"source"`
	Destination  string `json:"destination"`
	Revision     string `json:"revision"`
	Confirmation string `json:"confirmation"`
}
type mongoDatabaseInfo struct {
	Exists      bool              `json:"exists"`
	Collections []json.RawMessage `json:"collections"`
	Version     string            `json:"version"`
	Port        int               `json:"port"`
	URI         string            `json:"uri"`
}
type mongoRestorePreview struct {
	Backup   mongoBackup `json:"backup"`
	Target   string      `json:"target"`
	Exists   bool        `json:"exists"`
	Revision string      `json:"revision"`
}
type mongoImportPreview struct {
	Source    Path             `json:"source"`
	Info      mongoArchiveInfo `json:"info"`
	SizeBytes int64            `json:"sizeBytes"`
	SHA256    string           `json:"sha256"`
	Revision  string           `json:"revision"`
}

func mongoDatabaseName(name string) error {
	if !mongoName(name, 63) || strings.ContainsAny(name, "/\\.\"$*<>:|?") || strings.IndexFunc(name, unicode.IsSpace) >= 0 || slices.Contains([]string{"admin", "local", "config"}, strings.ToLower(name)) {
		return invalid("请选择业务数据库；名称不能包含空白或路径、命名空间特殊字符")
	}
	return nil
}

const mongoInspectScript = `
  const name = input.request.database;
  const names = checked(admin.runCommand({listDatabases:1,nameOnly:true,maxTimeMS:5000})).databases.map(d=>d.name);
  if (names.some(n => n.toLowerCase() === name.toLowerCase() && n !== name)) throw new Error('Database name differs only by case');
  const exists = names.includes(name);
  const database = connection.getDB(name);
  if (input.request.drop) checked(database.runCommand({dropDatabase:1}));
  const collections = exists && !input.request.drop ? database.getCollectionInfos().map(c=>({name:c.name,type:c.type,uuid:EJSON.stringify(c.info?.uuid ?? null)})).sort((a,b)=>a.name.localeCompare(b.name)) : [];
  print(JSON.stringify({result:{exists,collections,version:admin.version(),port:input.port,uri:input.uri}}));
`

func (c *Core) inspectMongoDatabase(ctx context.Context, version, database string, drop bool) (mongoDatabaseInfo, error) {
	var info mongoDatabaseInfo
	if err := mongoDatabaseName(database); err != nil {
		return info, err
	}
	credentials, err := c.mongoCredentials(ctx, version)
	if err != nil {
		return info, err
	}
	data, err := c.mongoExecute(ctx, version, map[string]any{"database": database, "drop": drop}, mongoInspectScript, credentials)
	if err != nil {
		return info, err
	}
	err = json.Unmarshal(data, &info)
	return info, err
}

func mongoExternalPath(path string, exporting bool) (string, error) {
	path = DisplayPath(path)
	if !filepath.IsAbs(path) {
		return "", invalid("请选择完整文件路径")
	}
	for _, p := range strings.FieldsFunc(path, func(r rune) bool { return r == '/' || r == '\\' }) {
		if p == ".." {
			return "", invalid("文件路径不能包含上级目录")
		}
	}
	path = filepath.Clean(path)
	start := path
	if exporting {
		start = filepath.Dir(path)
	}
	for current := start; ; current = filepath.Dir(current) {
		info, err := os.Lstat(current)
		if err != nil {
			return "", err
		}
		if info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return "", invalid("请选择原始文件目录，路径不能经过链接或目录联接")
		}
		if filepath.Dir(current) == current {
			break
		}
	}
	return path, nil
}

func (c *Core) mongoBackupDir() (string, error) {
	dir := filepath.Join(c.Home, "backup", "mongodb")
	return dir, managedDirectory(c.Home, dir)
}
func (c *Core) mongoBackupPath(id string) (string, error) {
	if !safePart(id) || len(id) > 100 {
		return "", invalid("MongoDB 备份标识无效")
	}
	dir, err := c.mongoBackupDir()
	if err != nil {
		return "", err
	}
	path, err := mongoExternalPath(filepath.Join(dir, id), false)
	if err != nil {
		return "", err
	}
	info, err := os.Stat(path)
	if err != nil {
		return "", err
	}
	if !info.IsDir() {
		return "", invalid("备份不是普通目录")
	}
	return path, nil
}
func (c *Core) mongoBackupMetadata(id string) (mongoBackup, string, error) {
	var item mongoBackup
	dir, err := c.mongoBackupPath(id)
	if err != nil {
		return item, dir, err
	}
	path, err := mongoExternalPath(filepath.Join(dir, "metadata.json"), false)
	if err != nil {
		return item, dir, err
	}
	file, err := os.Open(path)
	if err != nil {
		return item, dir, err
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, 16385))
	if err != nil {
		return item, dir, err
	}
	if len(data) > 16384 || json.Unmarshal(data, &item) != nil {
		return item, dir, invalid("MongoDB 备份记录损坏")
	}
	hash, hashErr := hex.DecodeString(item.SHA256)
	if item.ID != id || item.CreatedAt <= 0 || item.SizeBytes <= 0 || mongoDatabaseName(item.Database) != nil || hashErr != nil || len(hash) != 32 || !slices.Contains([]string{"manual", "before-restore", "imported", "automatic"}, item.Kind) {
		return item, dir, invalid("MongoDB 备份记录字段无效")
	}
	if _, err = mongoArchiveVersion(item.Version); err != nil {
		return item, dir, err
	}
	if _, err = mongoArchiveVersion(item.ToolsVersion); err != nil {
		return item, dir, err
	}
	return item, dir, nil
}
func validateMongoArchive(ctx context.Context, item mongoBackup, path string) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()
	info, err := inspectMongoArchive(ctx, f, "", io.Discard)
	if err != nil {
		return err
	}
	if info.Version != item.Version || info.ToolsVersion != item.ToolsVersion || info.Compression != "gzip" || len(info.Databases) != 1 || info.Databases[0].Name != item.Database {
		return invalid("备份记录与归档中的真实数据库或版本不一致")
	}
	return nil
}
func (c *Core) verifyMongoBackup(ctx context.Context, id string) (mongoBackup, string, error) {
	item, dir, err := c.mongoBackupMetadata(id)
	if err != nil {
		return item, dir, err
	}
	path := filepath.Join(dir, "archive.gz")
	digest, err := copyRDB(ctx, path, io.Discard, false)
	if err != nil {
		return item, dir, err
	}
	if digest.Size != item.SizeBytes || digest.Hash != item.SHA256 {
		return item, dir, invalid("MongoDB 备份摘要或大小不符")
	}
	return item, dir, validateMongoArchive(ctx, item, path)
}

func (c *Core) mongoBackupTool(ctx context.Context, name, version string) (Package, string, error) {
	var versions []string
	if version != "" {
		versions = []string{version}
	}
	p, entry, err := c.mongoTool(ctx, "mongodb-database-tools", versions...)
	if err != nil {
		return p, "", &AppError{Code: "MONGO_TOOLS_MISSING", Message: "请安装 MongoDB Database Tools；恢复需使用创建备份时的工具版本"}
	}
	path := filepath.Join(filepath.Dir(entry), name+filepath.Ext(entry))
	actual, err := filepath.EvalSymlinks(path)
	if err != nil {
		return p, "", err
	}
	root, err := filepath.EvalSymlinks(p.Install.InstallPath)
	if err != nil {
		return p, "", err
	}
	info, err := os.Stat(path)
	if err != nil {
		return p, "", err
	}
	if !pathContains(root, actual) || !info.Mode().IsRegular() {
		return p, "", invalid("MongoDB 工具入口无效")
	}
	return p, path, nil
}

func (c *Core) runMongoBackupTool(ctx context.Context, version, program, uri string, args []string) error {
	e, err := c.selectedMongo(version)
	if err != nil {
		return err
	}
	if err = c.verifyDatabaseListener(ctx, e); err != nil {
		return err
	}
	credentials, err := c.mongoCredentials(ctx, version)
	if err != nil {
		return err
	}
	dir, err := os.MkdirTemp(c.Home, ".mongo-tool-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(dir)
	all := []string{"--uri=" + uri + "/?directConnection=true&serverSelectionTimeoutMS=5000&connectTimeoutMS=5000"}
	if credentials.Username != "" {
		password, err := json.Marshal(credentials.Password)
		if err != nil {
			return err
		}
		config := filepath.Join(dir, "auth.yml")
		if err = os.WriteFile(config, append([]byte("password: "), password...), 0600); err != nil {
			return err
		}
		all = append(all, "--config="+DisplayPath(config), "--username="+credentials.Username, "--authenticationDatabase="+credentials.AuthDatabase)
	}
	all = append(all, args...)
	out := process.NewLimitedBuffer(65536)
	ctx, cancel := context.WithTimeout(ctx, 30*time.Minute)
	defer cancel()
	child, err := process.Start(ctx, process.Spec{Program: program, Args: all, Directory: dir, Environment: map[string]string{"HOME": dir, "USERPROFILE": dir}, Stdout: out, Stderr: out})
	if err != nil {
		return err
	}
	result := child.Wait()
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if result.Err != nil || result.Code != 0 {
		return &AppError{Code: "MONGO_BACKUP_TOOL_FAILED", Message: "MongoDB 备份工具执行失败，请检查工具版本和实例状态"}
	}
	return c.verifyDatabaseListener(ctx, e)
}

func (c *Core) publishMongoBackup(ctx context.Context, stage, database, version, tools, kind string) (mongoBackup, error) {
	item := mongoBackup{Database: database, Version: version, ToolsVersion: tools, Kind: kind, CreatedAt: time.Now().Unix()}
	id, err := newID("mongo")
	if err != nil {
		return item, err
	}
	item.ID = id
	digest, err := copyRDB(ctx, filepath.Join(stage, "archive.gz"), io.Discard, false)
	if err != nil {
		return item, err
	}
	item.SHA256 = digest.Hash
	item.SizeBytes = digest.Size
	if err = validateMongoArchive(ctx, item, filepath.Join(stage, "archive.gz")); err != nil {
		return item, err
	}
	data, err := json.Marshal(item)
	if err != nil {
		return item, err
	}
	if err = atomicWrite(filepath.Join(stage, "metadata.json"), data); err != nil {
		return item, err
	}
	dir, err := c.mongoBackupDir()
	if err != nil {
		return item, err
	}
	return item, os.Rename(stage, filepath.Join(dir, id))
}
func (c *Core) createMongoBackup(ctx context.Context, version, database, kind string) (mongoBackup, error) {
	var item mongoBackup
	info, err := c.inspectMongoDatabase(ctx, version, database, false)
	if err != nil {
		return item, err
	}
	if !info.Exists {
		return item, invalid("数据库已不存在")
	}
	tool, program, err := c.mongoBackupTool(ctx, "mongodump", "")
	if err != nil {
		return item, err
	}
	dir, err := c.mongoBackupDir()
	if err != nil {
		return item, err
	}
	stage, err := os.MkdirTemp(dir, ".pending-")
	if err != nil {
		return item, err
	}
	defer os.RemoveAll(stage)
	archive := filepath.Join(stage, "archive.gz")
	if err = c.runMongoBackupTool(ctx, version, program, info.URI, []string{"--db=" + database, "--archive=" + DisplayPath(archive), "--gzip"}); err != nil {
		return item, err
	}
	f, err := os.OpenFile(archive, os.O_RDWR, 0600)
	if err != nil {
		return item, err
	}
	err = errors.Join(f.Sync(), f.Close())
	if err != nil {
		return item, err
	}
	return c.publishMongoBackup(ctx, stage, database, info.Version, tool.Version, kind)
}

func (c *Core) mongoBackupList() (map[string]any, error) {
	dir, err := c.mongoBackupDir()
	if err != nil {
		return nil, err
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	items := []mongoBackup{}
	issues := []map[string]string{}
	for _, entry := range entries {
		if strings.HasPrefix(entry.Name(), ".") {
			continue
		}
		item, path, err := c.mongoBackupMetadata(entry.Name())
		if err == nil {
			info, statErr := os.Lstat(filepath.Join(path, "archive.gz"))
			if statErr != nil {
				err = statErr
			} else if !info.Mode().IsRegular() || info.Size() != item.SizeBytes {
				err = invalid("归档缺失或大小变化")
			}
		}
		if err != nil {
			issues = append(issues, map[string]string{"id": entry.Name(), "problem": publicError(err).Message})
		} else {
			items = append(items, item)
		}
	}
	sort.Slice(items, func(i, j int) bool {
		if items[i].CreatedAt == items[j].CreatedAt {
			return items[i].ID > items[j].ID
		}
		return items[i].CreatedAt > items[j].CreatedAt
	})
	return map[string]any{"items": items, "issues": issues, "unreadable": len(issues), "directory": Path(dir)}, nil
}

func (c *Core) mongoRemovalAt(ctx context.Context, id, dir string) (string, error) {
	if _, err := mongoExternalPath(dir, false); err != nil {
		return "", err
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return "", err
	}
	scope := map[string]any{}
	for _, entry := range entries {
		if entry.Name() != "metadata.json" && entry.Name() != "archive.gz" {
			return "", invalid("备份目录包含额外文件，未删除任何内容")
		}
		digest, err := copyRDB(ctx, filepath.Join(dir, entry.Name()), io.Discard, false)
		if err != nil {
			return "", err
		}
		scope[entry.Name()] = []any{digest.Size, digest.Hash}
	}
	return siteRevision(c.Home, id, scope), nil
}
func (c *Core) removeMongoBackup(ctx context.Context, id, revision string) error {
	dir, err := c.mongoBackupPath(id)
	if err != nil {
		return err
	}
	current, err := c.mongoRemovalAt(ctx, id, dir)
	if err != nil {
		return err
	}
	if revision == "" || current != revision {
		return invalid("备份已变化，请重新检查删除范围")
	}
	name, err := newID(".delete")
	if err != nil {
		return err
	}
	stage := filepath.Join(filepath.Dir(dir), name)
	if err = os.Rename(dir, stage); err != nil {
		return err
	}
	cleanup := func() error {
		current, err := c.mongoRemovalAt(ctx, id, stage)
		if err != nil {
			return err
		}
		if current != revision {
			return invalid("备份在删除前变化")
		}
		for _, name := range []string{"archive.gz", "metadata.json"} {
			if err = os.Remove(filepath.Join(stage, name)); err != nil && !os.IsNotExist(err) {
				return err
			}
		}
		return os.Remove(stage)
	}()
	if cleanup != nil {
		return errors.Join(cleanup, os.Rename(stage, dir))
	}
	return nil
}

func (c *Core) mongoImportPreview(ctx context.Context, source string) (mongoImportPreview, error) {
	var preview mongoImportPreview
	path, err := mongoExternalPath(source, false)
	if err != nil {
		return preview, err
	}
	digest, err := copyRDB(ctx, path, io.Discard, false)
	if err != nil {
		return preview, err
	}
	f, err := os.Open(path)
	if err != nil {
		return preview, err
	}
	info, err := inspectMongoArchive(ctx, f, "", io.Discard)
	f.Close()
	if err != nil {
		return preview, err
	}
	valid := false
	for _, db := range info.Databases {
		if mongoDatabaseName(db.Name) == nil {
			valid = true
		}
	}
	if !valid {
		return preview, invalid("归档没有可导入的业务数据库")
	}
	after, err := copyRDB(ctx, path, io.Discard, false)
	if err != nil {
		return preview, err
	}
	if after.Hash != digest.Hash || after.Size != digest.Size {
		return preview, invalid("归档在检查期间发生变化")
	}
	preview = mongoImportPreview{Source: Path(path), Info: info, SizeBytes: digest.Size, SHA256: digest.Hash}
	preview.Revision = siteRevision(c.Home, "mongo-import", preview)
	return preview, nil
}

func (c *Core) registerMongoBackups() {
	c.handle("mongodb_backup_list", func(context.Context, json.RawMessage) (any, error) { return c.mongoBackupList() })
	c.handle("mongodb_backup_create", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "create")
	})
	c.handle("mongodb_backup_inspect_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "inspect")
	})
	c.handle("mongodb_backup_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "import")
	})
	c.handle("mongodb_backup_export", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "export")
	})
	c.handle("mongodb_backup_removal_preview", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "removal")
	})
	c.handle("mongodb_backup_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "delete")
	})
	c.handle("mongodb_restore_preview", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "preview")
	})
	c.handle("mongodb_backup_restore", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "restore")
	})
	c.handle("mongodb_database_delete_preview", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "drop-preview")
	})
	c.handle("mongodb_database_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoBackupCommand(ctx, data, "drop")
	})
}

func (c *Core) mongoBackupCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[mongoBackupRequest](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	switch action {
	case "create":
		return c.createMongoBackup(ctx, in.Version, in.Database, "manual")
	case "inspect":
		return c.mongoImportPreview(ctx, in.Source)
	case "delete":
		return nil, c.removeMongoBackup(ctx, in.ID, in.Revision)
	case "removal":
		dir, err := c.mongoBackupPath(in.ID)
		if err != nil {
			return nil, err
		}
		revision, err := c.mongoRemovalAt(ctx, in.ID, dir)
		if err != nil {
			return nil, err
		}
		view := map[string]any{"id": in.ID, "database": nil, "kind": nil, "sizeBytes": nil, "revision": revision}
		if item, _, err := c.mongoBackupMetadata(in.ID); err == nil {
			view["database"] = item.Database
			view["kind"] = item.Kind
			view["sizeBytes"] = item.SizeBytes
		}
		return view, nil
	case "import":
		return c.importMongoBackup(ctx, in)
	case "export":
		return c.exportMongoBackup(ctx, in)
	case "preview", "restore":
		item, dir, err := c.verifyMongoBackup(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		preview, err := c.previewMongoRestore(ctx, in.Version, in.Target, item)
		if err != nil {
			return nil, err
		}
		if action == "preview" {
			return preview, nil
		}
		if in.Confirmation != in.Target || in.Revision == "" || in.Revision != preview.Revision {
			return nil, invalid("目标名称未确认或恢复范围已变化")
		}
		return c.restoreMongoBackup(ctx, in, preview, dir)
	case "drop-preview", "drop":
		info, err := c.inspectMongoDatabase(ctx, in.Version, in.Database, false)
		if err != nil {
			return nil, err
		}
		if !info.Exists {
			return nil, invalid("所选数据库已不存在")
		}
		e, err := c.selectedMongo(in.Version)
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		serviceRevision := e.revision
		c.services.mu.RUnlock()
		revision := siteRevision(c.Home, "mongo-drop", []any{in.Version, in.Database, serviceRevision, info})
		if action == "drop-preview" {
			return map[string]any{"version": in.Version, "database": in.Database, "collections": len(info.Collections), "revision": revision}, nil
		}
		if in.Confirmation != in.Database || in.Revision != revision {
			return nil, invalid("数据库名称未确认或删除范围已变化")
		}
		if _, err = c.inspectMongoDatabase(ctx, in.Version, in.Database, true); err != nil {
			return nil, err
		}
		after, err := c.inspectMongoDatabase(ctx, in.Version, in.Database, false)
		if err != nil {
			return nil, err
		}
		if after.Exists {
			return nil, invalid("MongoDB 未确认删除数据库")
		}
		return nil, nil
	}
	return nil, invalid("MongoDB 备份操作无效")
}

func (c *Core) importMongoBackup(ctx context.Context, in mongoBackupRequest) (any, error) {
	if err := mongoDatabaseName(in.Database); err != nil {
		return nil, err
	}
	preview, err := c.mongoImportPreview(ctx, in.Source)
	if err != nil {
		return nil, err
	}
	if in.Revision == "" || preview.Revision != in.Revision {
		return nil, invalid("源归档与预览不同")
	}
	dir, err := c.mongoBackupDir()
	if err != nil {
		return nil, err
	}
	stage, err := os.MkdirTemp(dir, ".pending-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(stage)
	source := filepath.Join(stage, "source.archive")
	f, err := os.OpenFile(source, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return nil, err
	}
	digest, copyErr := copyRDB(ctx, string(preview.Source), f, false)
	err = errors.Join(copyErr, f.Close())
	if err != nil {
		return nil, err
	}
	if digest.Hash != preview.SHA256 || digest.Size != preview.SizeBytes {
		return nil, invalid("源归档在复制期间变化")
	}
	input, err := os.Open(source)
	if err != nil {
		return nil, err
	}
	output, err := os.OpenFile(filepath.Join(stage, "archive.gz"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		input.Close()
		return nil, err
	}
	z := gzip.NewWriter(output)
	info, inspectErr := inspectMongoArchive(ctx, input, in.Database, z)
	err = errors.Join(inspectErr, z.Close(), output.Sync(), output.Close(), input.Close())
	if err != nil {
		return nil, err
	}
	if err = os.Remove(source); err != nil {
		return nil, err
	}
	return c.publishMongoBackup(ctx, stage, in.Database, info.Version, info.ToolsVersion, "imported")
}

func (c *Core) exportMongoBackup(ctx context.Context, in mongoBackupRequest) (any, error) {
	item, dir, err := c.verifyMongoBackup(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	dest, err := mongoExternalPath(in.Destination, true)
	if err != nil {
		return nil, err
	}
	if !strings.EqualFold(filepath.Ext(dest), ".gz") || pathContains(c.Home, dest) {
		return nil, invalid("请选择应用数据目录以外的 .gz 文件")
	}
	root, err := os.OpenRoot(filepath.Dir(dest))
	if err != nil {
		return nil, err
	}
	defer root.Close()
	name, err := newID(".mongo-export")
	if err != nil {
		return nil, err
	}
	f, err := root.OpenFile(name, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return nil, err
	}
	defer root.Remove(name)
	digest, copyErr := copyRDB(ctx, filepath.Join(dir, "archive.gz"), f, false)
	err = errors.Join(copyErr, f.Sync(), f.Close())
	if err != nil {
		return nil, err
	}
	if digest.Hash != item.SHA256 || digest.Size != item.SizeBytes {
		return nil, invalid("导出期间备份变化")
	}
	if err = root.Link(name, filepath.Base(dest)); err != nil {
		return nil, err
	}
	return Path(dest), nil
}

func (c *Core) previewMongoRestore(ctx context.Context, version, target string, item mongoBackup) (mongoRestorePreview, error) {
	preview := mongoRestorePreview{Backup: item, Target: target}
	info, err := c.inspectMongoDatabase(ctx, version, target, false)
	if err != nil {
		return preview, err
	}
	old, err := mongoArchiveVersion(item.Version)
	if err != nil {
		return preview, err
	}
	current, err := mongoArchiveVersion(info.Version)
	if err != nil {
		return preview, err
	}
	if old[0] != current[0] {
		return preview, invalid("备份与目标 MongoDB 主版本不同")
	}
	if _, _, err = c.mongoBackupTool(ctx, "mongorestore", item.ToolsVersion); err != nil {
		return preview, err
	}
	e, err := c.selectedMongo(version)
	if err != nil {
		return preview, err
	}
	c.services.mu.RLock()
	revision := e.revision
	c.services.mu.RUnlock()
	preview.Exists = info.Exists
	preview.Revision = siteRevision(c.Home, "mongo-restore", []any{item, target, info, revision})
	return preview, nil
}

func (c *Core) restoreMongoBackup(ctx context.Context, in mongoBackupRequest, preview mongoRestorePreview, dir string) (any, error) {
	stage, err := os.MkdirTemp(c.Home, ".mongo-restore-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(stage)
	archive := filepath.Join(stage, "archive.gz")
	f, err := os.OpenFile(archive, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return nil, err
	}
	digest, copyErr := copyRDB(ctx, filepath.Join(dir, "archive.gz"), f, false)
	err = errors.Join(copyErr, f.Sync(), f.Close())
	if err != nil {
		return nil, err
	}
	if digest.Hash != preview.Backup.SHA256 || digest.Size != preview.Backup.SizeBytes {
		return nil, invalid("备份在恢复前变化")
	}
	if err = validateMongoArchive(ctx, preview.Backup, archive); err != nil {
		return nil, err
	}
	_, program, err := c.mongoBackupTool(ctx, "mongorestore", preview.Backup.ToolsVersion)
	if err != nil {
		return nil, err
	}
	info, err := c.inspectMongoDatabase(ctx, in.Version, in.Target, false)
	if err != nil {
		return nil, err
	}
	args := []string{"--archive=" + DisplayPath(archive), "--gzip", "--stopOnError", "--nsInclude=" + preview.Backup.Database + ".*", "--nsFrom=" + preview.Backup.Database + ".*", "--nsTo=" + in.Target + ".*"}
	if err = c.runMongoBackupTool(ctx, in.Version, program, info.URI, append(append([]string{}, args...), "--dryRun")); err != nil {
		return nil, err
	}
	var safety *mongoBackup
	if preview.Exists {
		backup, err := c.createMongoBackup(ctx, in.Version, in.Target, "before-restore")
		if err != nil {
			return nil, err
		}
		safety = &backup
	}
	current, err := c.previewMongoRestore(ctx, in.Version, in.Target, preview.Backup)
	if err != nil {
		return nil, err
	}
	if current.Revision != in.Revision {
		return nil, invalid("目标在准备期间变化，已有保护备份仍保留")
	}
	writeErr := func() error {
		if preview.Exists {
			if _, err := c.inspectMongoDatabase(ctx, in.Version, in.Target, true); err != nil {
				return err
			}
		}
		if err := c.runMongoBackupTool(ctx, in.Version, program, info.URI, args); err != nil {
			return err
		}
		after, err := c.inspectMongoDatabase(ctx, in.Version, in.Target, false)
		if err != nil {
			return err
		}
		if !after.Exists {
			return invalid("恢复命令未创建目标数据库")
		}
		return nil
	}()
	if writeErr != nil {
		hint := "目标可能包含部分恢复数据，请暂停外部写入并检查后重新恢复。"
		if safety != nil {
			hint = fmt.Sprintf("目标可能已部分恢复，请暂停外部写入；保护备份 %s 已保留。", safety.ID)
		}
		return nil, &AppError{Code: "MONGO_RESTORE_INCOMPLETE", Message: "MongoDB 恢复未完成", Hint: hint, Detail: publicError(writeErr).Message}
	}
	return map[string]any{"target": in.Target, "safetyBackup": safety}, nil
}
