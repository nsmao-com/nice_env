// Package hostfile edits only the system hosts file through a dedicated helper.
package hostfile

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"
)

const Limit = 1024 * 1024
const HelperArgument = "--niceenv-apply-hosts"
const Begin = "# BEGIN NiceEnv (managed)"
const End = "# END NiceEnv (managed)"

var ErrChanged = errors.New("hosts 文件已变化，请重新读取后保存")

type Entry struct {
	IP      string `json:"ip"`
	Domain  string `json:"domain"`
	Managed bool   `json:"managed"`
}

func Normalize(ip, domain string) (Entry, error) {
	address, err := netip.ParseAddr(strings.TrimSpace(ip))
	if err != nil || address.Zone() != "" {
		return Entry{}, errors.New("hosts IP 地址格式无效")
	}
	domain = strings.TrimSuffix(strings.ToLower(strings.TrimSpace(domain)), ".")
	if len(domain) == 0 || len(domain) > 253 {
		return Entry{}, errors.New("hosts 主机名长度无效")
	}
	if _, err := netip.ParseAddr(domain); err == nil {
		return Entry{}, errors.New("hosts 主机名不能是 IP 地址")
	}
	for _, label := range strings.Split(domain, ".") {
		if len(label) == 0 || len(label) > 63 || label[0] == '-' || label[len(label)-1] == '-' {
			return Entry{}, errors.New("hosts 主机名格式无效")
		}
		for _, ch := range label {
			if !(ch >= 'a' && ch <= 'z' || ch >= '0' && ch <= '9' || ch == '-') {
				return Entry{}, errors.New("hosts 主机名不能包含协议、端口、空格或通配符")
			}
		}
	}
	return Entry{address.String(), domain, true}, nil
}

func marker(line string) int {
	switch strings.TrimSpace(strings.TrimPrefix(line, "\ufeff")) {
	case Begin, "# BEGIN NiceServBay (managed)":
		return 1
	case End, "# END NiceServBay (managed)":
		return -1
	}
	return 0
}
func blockStarts(lines []string) []bool {
	starts := make([]bool, len(lines))
	next := 0
	for i := len(lines) - 1; i >= 0; i-- {
		m := marker(lines[i])
		if m == 1 {
			starts[i] = next == -1
		}
		if m != 0 {
			next = m
		}
	}
	return starts
}

func Parse(content string, strict bool) ([]Entry, error) {
	if len(content) > Limit || !utf8.ValidString(content) || strings.ContainsRune(content, 0) {
		return nil, errors.New("hosts 必须为不超过 1 MiB 的有效 UTF-8 文本")
	}
	lines := strings.Split(strings.TrimPrefix(content, "\ufeff"), "\n")
	starts := blockStarts(lines)
	managed := false
	out := []Entry{}
	for i, line := range lines {
		switch marker(line) {
		case 1:
			managed = starts[i]
			continue
		case -1:
			managed = false
			continue
		}
		body, _, _ := strings.Cut(line, "#")
		words := strings.Fields(body)
		if len(words) == 0 {
			continue
		}
		if len(words) < 2 && (managed || strict) {
			return nil, fmt.Errorf("hosts 第 %d 行缺少主机名", i+1)
		}
		for _, domain := range words[1:] {
			e, err := Normalize(words[0], domain)
			if err != nil {
				if managed || strict {
					return nil, fmt.Errorf("hosts 第 %d 行：%w", i+1, err)
				}
				continue
			}
			e.Managed = managed
			out = append(out, e)
		}
	}
	return out, nil
}

// Preserve all bytes outside complete managed blocks, including BOM and CRLF.
func Merge(original string, entries []Entry) string {
	newline := "\n"
	if strings.Contains(original, "\r\n") {
		newline = "\r\n"
	}
	var block strings.Builder
	block.WriteString(Begin + newline)
	for _, e := range entries {
		fmt.Fprintf(&block, "%-15s %s%s", e.IP, e.Domain, newline)
	}
	block.WriteString(End + newline)
	bom := ""
	if strings.HasPrefix(original, "\ufeff") {
		bom = "\ufeff"
		original = strings.TrimPrefix(original, bom)
	}
	lines := strings.SplitAfter(original, "\n")
	starts := blockStarts(lines)
	var out strings.Builder
	out.WriteString(bom)
	inside, written := false, false
	for i, line := range lines {
		switch marker(line) {
		case 1:
			inside = starts[i]
			if !written {
				out.WriteString(block.String())
				written = true
			}
			continue
		case -1:
			inside = false
			continue
		}
		if !inside {
			out.WriteString(line)
		}
	}
	if !written {
		if len(entries) == 0 {
			return bom + original
		}
		if original != "" && !strings.HasSuffix(original, "\n") {
			out.WriteString(newline)
		}
		out.WriteString(block.String())
	}
	return out.String()
}

func Read(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() {
		return "", errors.New("hosts 不是普通文件")
	}
	data, err := io.ReadAll(io.LimitReader(f, Limit+1))
	if err != nil {
		return "", err
	}
	if len(data) > Limit || !utf8.Valid(data) || bytes.ContainsRune(data, 0) {
		return "", errors.New("hosts 文件过大或编码无效，未修改")
	}
	return string(data), nil
}

// WriteSnapshot keeps the original file identity and ACL. The recovery copy is
// durable before truncation, and remains on disk if restoration is required.
func WriteSnapshot(path, expected, content string) error {
	if _, err := Parse(content, true); err != nil {
		return err
	}
	f, err := openExclusive(path)
	if err != nil {
		return err
	}
	defer f.Close()
	current, err := io.ReadAll(io.LimitReader(f, Limit+1))
	if err != nil {
		return err
	}
	if string(current) != expected {
		return ErrChanged
	}
	if content == expected {
		return nil
	}
	backup, err := os.CreateTemp(filepath.Dir(path), "niceenv-hosts-*.bak")
	if err != nil {
		return err
	}
	_, writeErr := backup.Write(current)
	syncErr := backup.Sync()
	closeErr := backup.Close()
	if err = errors.Join(writeErr, syncErr, closeErr); err != nil {
		return err
	}
	write := func(data []byte) error {
		if _, err := f.Seek(0, io.SeekStart); err != nil {
			return err
		}
		if _, err := f.Write(data); err != nil {
			return err
		}
		if err := f.Truncate(int64(len(data))); err != nil {
			return err
		}
		return f.Sync()
	}
	if err = write([]byte(content)); err != nil {
		restore := write(current)
		return fmt.Errorf("hosts 写入失败：%w；恢复结果：%v；恢复副本：%s", err, restore, backup.Name())
	}
	_ = os.Remove(backup.Name())
	return nil
}

type request struct {
	Expected string `json:"expected"`
	Content  string `json:"content"`
}

// RunHelper must be called before opening Wails, settings, or any service. The
// request cannot select a target path or execute another command.
func RunHelper(args []string) (bool, error) {
	if len(args) == 0 || args[0] != HelperArgument {
		return false, nil
	}
	if len(args) != 2 {
		return true, errors.New("hosts 授权参数无效")
	}
	f, err := os.Open(args[1])
	if err != nil {
		return true, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, 8*Limit+1))
	if err != nil {
		return true, err
	}
	if len(data) > 8*Limit {
		return true, errors.New("hosts 授权请求过大")
	}
	var req request
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	if err = dec.Decode(&req); err != nil {
		return true, err
	}
	if dec.Decode(new(any)) != io.EOF {
		return true, errors.New("hosts 授权请求格式无效")
	}
	if len(req.Expected) > Limit {
		return true, errors.New("hosts 原始快照过大")
	}
	path, err := SystemPath()
	if err != nil {
		return true, err
	}
	return true, WriteSnapshot(path, req.Expected, req.Content)
}

func Save(path, expected, content string, allowElevation bool) error {
	err := WriteSnapshot(path, expected, content)
	if err == nil || !allowElevation || !os.IsPermission(err) {
		return err
	}
	system, sysErr := SystemPath()
	if sysErr != nil {
		return sysErr
	}
	if path != system {
		return errors.New("仅系统 hosts 可以请求管理员授权")
	}
	data, err := json.Marshal(request{expected, content})
	if err != nil {
		return err
	}
	f, err := os.CreateTemp("", "niceenv-hosts-request-*.json")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	_, writeErr := f.Write(data)
	syncErr := f.Sync()
	closeErr := f.Close()
	if err = errors.Join(writeErr, syncErr, closeErr); err != nil {
		return err
	}
	if err = elevate(f.Name()); err != nil {
		return err
	}
	actual, err := Read(path)
	if err != nil {
		return err
	}
	if actual != content {
		return errors.New("hosts 授权程序已退出，但写入结果未通过核对，请重新读取")
	}
	return nil
}
