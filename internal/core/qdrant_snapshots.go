package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"

	"niceenv/internal/process"
)

type qdrantSnapshot struct {
	name, digest string
	source       *os.Root
}

func qdrantSnapshotDigest(ctx context.Context, root *os.Root, name string) (string, error) {
	info, err := siteCheck(root, name, false)
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() {
		return "", invalid("Qdrant 快照包含非普通文件")
	}
	f, err := root.Open(name)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha256.New()
	_, err = io.Copy(h, contextReader{ctx, f})
	if err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// First validate all source/destination collisions, then publish complete files
// without replacement. Source directories are archived only after every file
// is verified. Interrupted copies are safe to retry and never become snapshots.
func (c *Core) preserveQdrantSnapshots(ctx context.Context) error {
	c.services.mu.RLock()
	busy := false
	for _, e := range c.services.entries {
		if e.pkg.ID == "qdrant" && e.child != nil {
			busy = true
		}
	}
	c.services.mu.RUnlock()
	if busy {
		return serviceBusy()
	}
	locks := filepath.Join(c.Home, ".locks")
	if err := managedDirectory(c.Home, locks); err != nil {
		return err
	}
	unlock, err := process.Lock(filepath.Join(locks, "qdrant-snapshots.lock"))
	if errors.Is(err, process.ErrLocked) {
		return serviceBusy()
	}
	if err != nil {
		return err
	}
	defer unlock()
	packages, err := c.qdrantPackages(ctx)
	if err != nil {
		return err
	}
	type sourceDir struct {
		root          *os.Root
		path, version string
		files         []qdrantSnapshot
	}
	sources := []sourceDir{}
	defer func() {
		for _, s := range sources {
			s.root.Close()
		}
	}()
	plan := map[string]qdrantSnapshot{}
	for _, p := range packages {
		if p.Run == nil || !reflect.DeepEqual(p.Run.Args, qdrantArgs) || p.Run.Cwd != "" || p.Run.DataDir != "" {
			continue
		}
		program, err := relativePath(p.Install.InstallPath, p.Entry)
		if err != nil {
			return err
		}
		source := filepath.Join(filepath.Dir(program), "snapshots")
		if !pathContains(c.Home, source) {
			return invalid("Qdrant 快照来源超出应用目录")
		}
		if _, err := os.Lstat(source); os.IsNotExist(err) {
			continue
		} else if err != nil {
			return err
		}
		root, err := siteBackupRoot(source)
		if err != nil {
			return err
		}
		sources = append(sources, sourceDir{root: root, path: source, version: p.Version})
		s := &sources[len(sources)-1]
		err = fs.WalkDir(root.FS(), ".", func(name string, d fs.DirEntry, walkErr error) error {
			if walkErr != nil {
				return walkErr
			}
			if err := ctx.Err(); err != nil {
				return err
			}
			if name == "." {
				return nil
			}
			if len(plan) >= 100000 || strings.Count(filepath.ToSlash(name), "/") > 32 {
				return invalid("Qdrant 快照文件数或层级超过限制")
			}
			name = filepath.FromSlash(name)
			if _, err := siteCheck(root, name, false); err != nil {
				return err
			}
			if d.IsDir() {
				return nil
			}
			digest, err := qdrantSnapshotDigest(ctx, root, name)
			if err != nil {
				return err
			}
			item := qdrantSnapshot{name: name, digest: digest, source: root}
			key := qdrantPathKey(name)
			if previous, exists := plan[key]; exists && previous.digest != digest {
				return &AppError{Code: "QDRANT_SNAPSHOT_CONFLICT", Message: "同名快照内容不同，原目录已保留", Detail: name}
			}
			plan[key] = item
			s.files = append(s.files, item)
			return nil
		})
		if err != nil {
			return err
		}
	}
	if len(plan) == 0 {
		return nil
	}
	target := filepath.Join(c.Home, "data", "qdrant", "snapshots")
	if err := managedDirectory(c.Home, target); err != nil {
		return err
	}
	dest, err := siteBackupRoot(target)
	if err != nil {
		return err
	}
	defer dest.Close()
	items := make([]qdrantSnapshot, 0, len(plan))
	for _, item := range plan {
		for parent := filepath.Dir(item.name); parent != "."; parent = filepath.Dir(parent) {
			if _, found := plan[qdrantPathKey(parent)]; found {
				return invalid("Qdrant 快照文件与目录名称冲突")
			}
		}
		if err := qdrantSnapshotDestination(dest, item.name); err != nil {
			return err
		}
		if _, err := dest.Lstat(item.name); err == nil {
			digest, err := qdrantSnapshotDigest(ctx, dest, item.name)
			if err != nil {
				return err
			}
			if digest != item.digest {
				return &AppError{Code: "QDRANT_SNAPSHOT_CONFLICT", Message: "目标已有不同内容的快照，未覆盖", Detail: item.name}
			}
		} else if !os.IsNotExist(err) {
			return err
		}
		items = append(items, item)
	}
	sort.Slice(items, func(i, j int) bool { return items[i].name < items[j].name })
	stage, err := os.MkdirTemp(filepath.Dir(target), ".qdrant-snapshots-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(stage) // invocation-owned sibling, never a user directory
	for i, item := range items {
		if _, err := dest.Stat(item.name); err == nil {
			continue
		}
		if err := siteBackupRootUnchanged(dest, target); err != nil {
			return err
		}
		input, err := item.source.Open(item.name)
		if err != nil {
			return err
		}
		temp, err := os.CreateTemp(stage, "copy-")
		if err != nil {
			input.Close()
			return err
		}
		h := sha256.New()
		_, copyErr := io.Copy(io.MultiWriter(temp, h), contextReader{ctx, input})
		input.Close()
		syncErr := temp.Sync()
		closeErr := temp.Close()
		if err := errors.Join(copyErr, syncErr, closeErr); err != nil {
			return err
		}
		if hex.EncodeToString(h.Sum(nil)) != item.digest {
			return invalid("复制期间 Qdrant 快照已变化，原目录已保留")
		}
		if err := dest.MkdirAll(filepath.Dir(item.name), 0700); err != nil {
			return err
		}
		if _, err := siteCheck(dest, item.name, true); err != nil {
			return err
		}
		// Hard-link publication is atomic and refuses an existing destination.
		if err := os.Link(temp.Name(), filepath.Join(target, item.name)); err != nil {
			return err
		}
		if c.emit != nil && i%100 == 0 {
			c.installProgress("qdrant-snapshots", "", "configuring", int64(i), int64(len(items)), 0, "")
		}
	}
	for _, source := range sources {
		for _, item := range source.files {
			digest, err := qdrantSnapshotDigest(ctx, source.root, item.name)
			if err != nil {
				return err
			}
			if digest != item.digest {
				return invalid("复制期间来源快照已变化，原目录已保留")
			}
			actual, err := qdrantSnapshotDigest(ctx, dest, item.name)
			if err != nil {
				return err
			}
			if actual != item.digest {
				return invalid("目标快照已变化，原目录已保留")
			}
		}
		if err := siteBackupRootUnchanged(source.root, source.path); err != nil {
			return err
		}
	}
	backup := filepath.Join(c.Home, "backup")
	if err := managedDirectory(c.Home, backup); err != nil {
		return err
	}
	for _, source := range sources {
		if len(source.files) == 0 {
			continue
		}
		directory, err := os.MkdirTemp(backup, "qdrant-snapshots-"+source.version+"-")
		if err != nil {
			return err
		}
		// Windows cannot rename while an os.Root handle pins the directory.
		if err := source.root.Close(); err != nil {
			return err
		}
		if err := os.Rename(source.path, filepath.Join(directory, "snapshots")); err != nil {
			return err
		}
	}
	return nil
}

func qdrantSnapshotDestination(root *os.Root, name string) error {
	current := "."
	parts := strings.Split(filepath.FromSlash(name), string(filepath.Separator))
	for i, part := range parts {
		current = filepath.Join(current, part)
		info, err := root.Lstat(current)
		if os.IsNotExist(err) {
			return nil
		}
		if err != nil {
			return err
		}
		if info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 || i < len(parts)-1 && !info.IsDir() {
			return invalid("Qdrant 快照目标包含链接或非目录父路径")
		}
	}
	return nil
}
