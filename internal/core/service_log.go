package core

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

const serviceLogLimit int64 = 4 * 1024 * 1024

func (c *Core) logSourcePath(ctx context.Context, id string) (string, error) {
	kind, siteID := "", ""
	if strings.HasPrefix(id, "site:") {
		kind, siteID = "access", strings.TrimPrefix(id, "site:")
	}
	if strings.HasPrefix(id, "site-error:") {
		kind, siteID = "error", strings.TrimPrefix(id, "site-error:")
	}
	if kind != "" {
		site, err := c.site(ctx, siteID)
		if err != nil {
			return "", err
		}
		server := site.Runtime.WebServer
		if server != "nginx" && server != "apache" && server != "caddy" {
			return "", invalid("站点 Web 服务配置无效")
		}
		return filepath.Join(c.Home, "logs", server, site.ID+"."+kind+".log"), nil
	}
	e, err := c.serviceEntry(id, "")
	if err != nil {
		return "", err
	}
	c.services.mu.RLock()
	defer c.services.mu.RUnlock()
	return string(e.status.LogFile), nil
}

// Keep draining on disk errors; report the first error when the process exits.
type serviceLog struct {
	mu   sync.Mutex
	file *os.File
	path string
	size int64
	err  error
}

func openServiceLog(path string) (*serviceLog, error) {
	if info, err := os.Lstat(path); err == nil && (!info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0) {
		return nil, invalid("日志目标不是普通文件")
	}
	file, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		return nil, err
	}
	info, err := file.Stat()
	if err != nil {
		file.Close()
		return nil, err
	}
	return &serviceLog{file: file, path: path, size: info.Size()}, nil
}
func (l *serviceLog) Write(data []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	total := len(data)
	for len(data) > 0 && l.err == nil {
		if l.size >= serviceLogLimit {
			if l.err = l.file.Close(); l.err != nil {
				break
			}
			old := l.path + ".1"
			if info, err := os.Lstat(old); err == nil {
				if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 {
					l.err = invalid("日志归档目标不是普通文件")
					break
				}
				l.err = os.Remove(old)
			} else if !os.IsNotExist(err) {
				l.err = err
			}
			if l.err != nil {
				break
			}
			if l.err = os.Rename(l.path, old); l.err != nil {
				break
			}
			l.file, l.err = os.OpenFile(l.path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
			l.size = 0
			if l.err != nil {
				break
			}
		}
		n := min(len(data), int(serviceLogLimit-l.size))
		written, err := l.file.Write(data[:n])
		l.size += int64(written)
		data = data[written:]
		l.err = err
		if written == 0 && err == nil {
			l.err = io.ErrShortWrite
		}
	}
	return total, nil
}
func (l *serviceLog) Close() error {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.file == nil {
		return l.err
	}
	return errors.Join(l.err, l.file.Close())
}

// Export a byte snapshot of the current file without passing log contents through
// the WebView or the bounded UTF-8 configuration reader. Appends after opening are
// left for the next export; rotation keeps this file handle valid.
func exportServiceLog(ctx context.Context, path, dest string) (int64, error) {
	if !filepath.IsAbs(dest) {
		return 0, invalid("请选择日志导出绝对路径")
	}
	if sameLocalPath(path, dest) {
		return 0, invalid("导出目标不能覆盖正在读取的日志")
	}
	if err := ctx.Err(); err != nil {
		return 0, err
	}
	before, err := os.Lstat(path)
	if os.IsNotExist(err) {
		return 0, atomicWrite(dest, nil)
	}
	if err != nil {
		return 0, err
	}
	if !before.Mode().IsRegular() {
		return 0, invalid("日志不是普通文件")
	}
	input, err := os.Open(path)
	if err != nil {
		return 0, err
	}
	defer input.Close()
	info, err := input.Stat()
	if err != nil {
		return 0, err
	}
	if !info.Mode().IsRegular() || !os.SameFile(before, info) {
		return 0, invalid("日志文件已变化，请重新导出")
	}
	if target, err := os.Stat(dest); err == nil && os.SameFile(info, target) {
		return 0, invalid("导出目标不能覆盖正在读取的日志")
	} else if err != nil && !os.IsNotExist(err) {
		return 0, err
	}
	var size int64
	err = atomicWriteWith(dest, func(output *os.File) error {
		var err error
		size, err = io.CopyN(output, contextReader{ctx, input}, info.Size())
		if err != nil {
			return err
		}
		return ctx.Err()
	})
	return size, err
}

func readServiceLog(path string, lines int) ([]map[string]any, error) {
	result := []map[string]any{}
	file, err := os.Open(path)
	if os.IsNotExist(err) {
		return result, nil
	}
	if err != nil {
		return nil, err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, invalid("日志不是普通文件")
	}
	const limit int64 = 1024 * 1024
	start := max(int64(0), info.Size()-limit)
	if _, err = file.Seek(start, io.SeekStart); err != nil {
		return nil, err
	}
	data, err := io.ReadAll(io.LimitReader(file, limit))
	if err != nil {
		return nil, err
	}
	if start > 0 {
		if i := bytes.IndexByte(data, '\n'); i >= 0 {
			data = data[i+1:]
		} else {
			data = nil
		}
	}
	text := strings.TrimRight(strings.ToValidUTF8(string(data), "�"), "\r\n")
	if text == "" {
		return result, nil
	}
	parts := strings.Split(text, "\n")
	parts = parts[max(0, len(parts)-lines):]
	for _, line := range parts {
		result = append(result, map[string]any{"line": strings.TrimSuffix(line, "\r")})
	}
	return result, nil
}
