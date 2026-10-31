package core

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"
)

type sftpgoConfigDirectory struct {
	Directory  string   `json:"directory"`
	Label      string   `json:"label"`
	ConfigFile *string  `json:"configFile"`
	StateFiles []string `json:"stateFiles"`
	ModifiedAt *int64   `json:"modifiedAt"`
	Issue      *string  `json:"issue"`
}

func sftpgoRelativeDirectory(home, path string) (string, error) {
	rel, err := filepath.Rel(home, path)
	if err != nil || !filepath.IsLocal(rel) {
		return "", invalid("SFTPGo 配置目录必须位于应用数据目录")
	}
	rel = filepath.ToSlash(rel)
	parts := strings.Split(rel, "/")
	if len(parts) != 3 || parts[0] != "etc" || parts[1] != "sftpgo" || !safePart(parts[2]) {
		return "", invalid("SFTPGo 配置目录无效")
	}
	return rel, nil
}
func (c *Core) sftpgoConfigDirectories(ctx context.Context) (any, error) {
	e, err := c.serviceEntry("sftpgo", "")
	if err != nil {
		return nil, err
	}
	current, _, err := c.Store.setting(ctx, "sftpgoConfigDir")
	if err != nil {
		return nil, err
	}
	parent := filepath.Join(c.Home, "etc", "sftpgo")
	if err = managedDirectory(c.Home, parent); err != nil {
		return nil, err
	}
	entries, err := os.ReadDir(parent)
	if err != nil {
		return nil, err
	}
	result := []sftpgoConfigDirectory{}
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		path := filepath.Join(parent, entry.Name())
		relative, err := sftpgoRelativeDirectory(c.Home, path)
		if err != nil {
			continue
		}
		files, err := os.ReadDir(path)
		if err != nil {
			continue
		}
		if len(files) == 0 && relative != current {
			continue
		}
		row := sftpgoConfigDirectory{Directory: relative, Label: entry.Name(), StateFiles: []string{}}
		var latest int64
		for _, file := range files {
			if file.IsDir() {
				continue
			}
			name := file.Name()
			info, statErr := file.Info()
			if statErr == nil {
				if value := info.ModTime().Unix(); value > latest {
					latest = value
				}
			}
			if strings.HasPrefix(name, "sftpgo.") && slices.Contains([]string{"json", "yaml", "yml", "toml", "hcl", "ini", "properties"}, strings.TrimPrefix(filepath.Ext(name), ".")) {
				if row.ConfigFile != nil {
					issue := "同一目录包含多个 SFTPGo 配置文件"
					row.Issue = &issue
				} else {
					value := name
					row.ConfigFile = &value
				}
			}
			if strings.HasSuffix(strings.ToLower(name), ".db") || strings.Contains(strings.ToLower(name), "host") || strings.Contains(strings.ToLower(name), "key") {
				row.StateFiles = append(row.StateFiles, name)
			}
		}
		if latest > 0 {
			row.ModifiedAt = &latest
		}
		r, inspectErr := c.resolveService(ctx, e)
		if inspectErr == nil {
			r.etc = path
			var state []string
			state, inspectErr = c.inspectSftpgo(ctx, e, r, true, false)
			if inspectErr == nil {
				row.StateFiles = state
			}
		}
		if inspectErr != nil {
			message := publicError(inspectErr).Message
			row.Issue = &message
		}
		result = append(result, row)
	}
	sort.Slice(result, func(i, j int) bool { return result[i].Directory < result[j].Directory })
	version := e.status.Version
	return map[string]any{"version": version, "current": nullableString(current), "directories": result}, nil
}
func nullableString(value string) any {
	if value == "" {
		return nil
	}
	return value
}

func (c *Core) selectSftpgoConfig(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Directory       string  `json:"directory"`
		Version         string  `json:"version"`
		ExpectedCurrent *string `json:"expectedCurrent"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !safePart(in.Version) {
		return nil, invalid("SFTPGo 版本无效")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	e, err := c.serviceEntry("sftpgo", in.Version)
	if err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	running := e.child != nil
	c.services.mu.RUnlock()
	if running {
		return nil, serviceBusy()
	}
	latest, err := c.sftpgoConfigDirectories(ctx)
	if err != nil {
		return nil, err
	}
	value, _ := json.Marshal(latest)
	var view struct {
		Version     string                  `json:"version"`
		Current     *string                 `json:"current"`
		Directories []sftpgoConfigDirectory `json:"directories"`
	}
	if err = json.Unmarshal(value, &view); err != nil {
		return nil, err
	}
	current, expected := "", ""
	if view.Current != nil {
		current = *view.Current
	}
	if in.ExpectedCurrent != nil {
		expected = *in.ExpectedCurrent
	}
	if view.Version != in.Version || current != expected && current != in.Directory {
		return nil, &AppError{Code: "SFTPGO_CONFIG_CHANGED", Message: "SFTPGo 版本或目录选择已经变化，请刷新后重试"}
	}
	var selected *sftpgoConfigDirectory
	for i := range view.Directories {
		if view.Directories[i].Directory == in.Directory {
			selected = &view.Directories[i]
			break
		}
	}
	if selected == nil {
		return nil, &AppError{Code: "SFTPGO_CONFIG_MISSING", Message: "所选配置目录不存在，请刷新后重试"}
	}
	if selected.Issue != nil {
		return nil, &AppError{Code: "SFTPGO_CONFIG_INVALID", Message: "所选目录未通过启动前检查", Hint: *selected.Issue}
	}
	if err = c.Store.saveSetting(ctx, "sftpgoConfigDir", in.Directory); err != nil {
		return nil, err
	}
	return true, nil
}

func (c *Core) registerSftpgo() {
	c.handle("sftpgo_config_directories", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.sftpgoConfigDirectories(ctx) })
	c.handle("select_sftpgo_config", c.selectSftpgoConfig)
}
