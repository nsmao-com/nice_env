package core

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

const siteIniBegin = "; BEGIN NiceEnv PHP settings"
const siteIniEnd = "; END NiceEnv PHP settings"

var siteIniSize = regexp.MustCompile(`^([0-9]+)([KkMmGg]?)$`)
var siteIniKey = regexp.MustCompile(`^[A-Za-z0-9_.]+$`)
var siteIniNumber = regexp.MustCompile(`^[0-9]+$`)

func validateSitePHP(site Site, previous *Site) error {
	if site.Runtime.Kind != "php" {
		return nil
	}
	for key, value := range site.PHPOverrides {
		valid := false
		number, err := strconv.ParseUint(value, 10, 32)
		numeric := err == nil && siteIniNumber.MatchString(value)
		switch key {
		case "memory_limit", "upload_max_filesize", "post_max_size":
			if value == "-1" && key == "memory_limit" {
				valid = true
				break
			}
			match := siteIniSize.FindStringSubmatch(value)
			if match != nil {
				n, err := strconv.ParseUint(match[1], 10, 64)
				shift := map[string]uint{"K": 10, "M": 20, "G": 30}[strings.ToUpper(match[2])]
				valid = err == nil && n <= uint64(1<<63-1)>>shift && (key != "memory_limit" || n<<shift >= 2*1024*1024)
			}
		case "max_execution_time":
			valid = numeric
		case "max_input_time":
			valid = numeric || value == "-1"
		case "max_input_vars", "max_file_uploads":
			valid = numeric && number > 0
		case "display_errors", "log_errors":
			valid = value == "1" || value == "0" || strings.EqualFold(value, "on") || strings.EqualFold(value, "off")
		default:
			valid = previous != nil && previous.PHPOverrides[key] == value && siteIniKey.MatchString(key) && value != "" && !strings.ContainsAny(value, `'";$[]`)
		}
		if !valid || strings.IndexFunc(value, func(r rune) bool { return unicode.IsControl(r) || unicode.IsSpace(r) }) >= 0 {
			return invalid("PHP 设置 " + key + " 不受支持或值无效")
		}
	}
	return nil
}
func sortedIni(values map[string]string) string {
	keys := []string{}
	for key := range values {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	var out strings.Builder
	for _, key := range keys {
		fmt.Fprintf(&out, "%s=%s\n", key, values[key])
	}
	return out.String()
}
func renderSiteIni(before []byte, exists bool, overrides, previous map[string]string) ([]byte, bool, error) {
	if !utf8.Valid(before) {
		return nil, false, invalid(".user.ini 不是 UTF-8 文本，未修改原文件")
	}
	raw := string(before)
	inside, seen := false, false
	var unmanaged strings.Builder
	for _, line := range strings.SplitAfter(raw, "\n") {
		switch strings.TrimRight(line, "\r\n") {
		case siteIniBegin:
			if inside || seen {
				return nil, false, invalid(".user.ini 托管标记重复")
			}
			inside = true
			seen = true
		case siteIniEnd:
			if !inside {
				return nil, false, invalid(".user.ini 托管标记不完整")
			}
			inside = false
		default:
			if !inside {
				unmanaged.WriteString(line)
			}
		}
	}
	if inside {
		return nil, false, invalid(".user.ini 托管区未结束")
	}
	legacy := strings.HasPrefix(raw, "; NiceEnv managed .user.ini\n")
	if legacy {
		if raw != "; NiceEnv managed .user.ini\n"+sortedIni(previous) {
			return nil, false, invalid("旧版 .user.ini 已手动修改，请先备份并移除旧版 managed 标记")
		}
		unmanaged.Reset()
	}
	if len(overrides) == 0 {
		if !seen && !legacy {
			return before, exists, nil
		}
		return []byte(unmanaged.String()), unmanaged.Len() > 0, nil
	}
	if unmanaged.Len() > 0 && !strings.HasSuffix(unmanaged.String(), "\n") {
		unmanaged.WriteByte('\n')
	}
	newline := "\n"
	if strings.Contains(raw, "\r\n") {
		newline = "\r\n"
	}
	unmanaged.WriteString(siteIniBegin + newline + strings.ReplaceAll(sortedIni(overrides), "\n", newline) + siteIniEnd + newline)
	return []byte(unmanaged.String()), true, nil
}

func (c *Core) prepareSiteIni(ctx context.Context, journal *siteFileJournal, site Site, previous *Site) error {
	if err := validateSitePHP(site, previous); err != nil {
		return err
	}
	others, err := c.sites(ctx)
	if err != nil {
		return err
	}
	sameRoot := func(a, b string) bool {
		first, e1 := filepath.EvalSymlinks(a)
		second, e2 := filepath.EvalSymlinks(b)
		if e1 != nil || e2 != nil {
			return false
		}
		rel, err := filepath.Rel(first, second)
		return err == nil && rel == "."
	}
	add := func(dir string, values, old map[string]string) error {
		path, err := filepath.EvalSymlinks(DisplayPath(dir))
		if err != nil {
			return err
		}
		root, err := os.OpenRoot(path)
		if err != nil {
			return err
		}
		defer root.Close()
		before, exists, err := envRead(root, ".user.ini")
		if err != nil {
			return err
		}
		after, present, err := renderSiteIni(before, exists, values, old)
		if err != nil {
			return err
		}
		return journal.add(filepath.Join(path, ".user.ini"), after, present)
	}
	oldValues := map[string]string{}
	if previous != nil && previous.Runtime.Kind == "php" {
		oldValues = previous.PHPOverrides
		if len(oldValues) > 0 && (site.Runtime.Kind != "php" || !sameRoot(string(previous.RootDir), string(site.RootDir))) {
			shared := false
			for _, other := range others {
				shared = shared || other.ID != site.ID && other.Runtime.Kind == "php" && sameRoot(string(other.RootDir), string(previous.RootDir))
			}
			if info, err := os.Stat(string(previous.RootDir)); err == nil && info.IsDir() && !shared {
				if err = add(string(previous.RootDir), nil, oldValues); err != nil {
					return err
				}
			}
		}
	}
	if site.Runtime.Kind == "php" && (len(site.PHPOverrides) > 0 || len(oldValues) > 0) {
		for _, other := range others {
			if other.ID != site.ID && other.Runtime.Kind == "php" && sameRoot(string(other.RootDir), string(site.RootDir)) && sortedIni(other.PHPOverrides) != sortedIni(site.PHPOverrides) {
				return invalid("多个 PHP 站点共用根目录，不能应用不同的 PHP 设置")
			}
		}
		return add(string(site.RootDir), site.PHPOverrides, oldValues)
	}
	return nil
}
