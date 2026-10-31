package core

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"unicode/utf8"
)

const maxTextFile = 8 * 1024 * 1024

func (c *Core) registerFiles() {
	c.handle("read_text_file", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			Path string `json:"path"`
		}](data)
		if err != nil {
			return nil, err
		}
		if args.Path == "" {
			return nil, invalid("请选择文件")
		}
		file, err := os.Open(args.Path)
		if err != nil {
			return nil, err
		}
		defer file.Close()
		info, err := file.Stat()
		if err != nil {
			return nil, err
		}
		if !info.Mode().IsRegular() || info.Size() > maxTextFile {
			return nil, invalid("请选择不超过 8 MiB 的文本文件")
		}
		text, err := io.ReadAll(io.LimitReader(file, maxTextFile+1))
		if err != nil {
			return nil, err
		}
		if len(text) > maxTextFile || !utf8.Valid(text) {
			return nil, invalid("文件过大或不是 UTF-8 文本")
		}
		return string(text), ctx.Err()
	})
	c.handle("write_text_file", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			Path    string `json:"path"`
			Content string `json:"content"`
		}](data)
		if err != nil {
			return nil, err
		}
		if args.Path == "" || len(args.Content) > maxTextFile {
			return nil, invalid("保存路径或内容无效")
		}
		if err = ctx.Err(); err != nil {
			return nil, err
		}
		return true, atomicWrite(args.Path, []byte(args.Content))
	})
}

// Write in the destination directory so rename cannot cross volumes. A failed
// write leaves the old file intact and removes only our own temporary file.
func atomicWrite(path string, data []byte) error {
	mode := os.FileMode(0600)
	if info, err := os.Lstat(path); err == nil {
		if !info.Mode().IsRegular() {
			return invalid("目标必须是普通文件，不能是符号链接或目录")
		}
		mode = info.Mode().Perm()
	} else if !os.IsNotExist(err) {
		return err
	}
	file, err := os.CreateTemp(filepath.Dir(path), ".niceenv-write-*")
	if err != nil {
		return err
	}
	name := file.Name()
	defer os.Remove(name)
	if err = file.Chmod(mode); err != nil {
		file.Close()
		return err
	}
	if _, err = file.Write(data); err != nil {
		file.Close()
		return err
	}
	if err = file.Sync(); err != nil {
		file.Close()
		return err
	}
	if err = file.Close(); err != nil {
		return err
	}
	return os.Rename(name, path)
}
