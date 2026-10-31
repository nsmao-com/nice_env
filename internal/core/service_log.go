package core

import (
	"bytes"
	"errors"
	"io"
	"os"
	"strings"
	"sync"
)

const serviceLogLimit int64 = 4 * 1024 * 1024

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
