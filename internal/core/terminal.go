package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"unicode"
)

type terminalEntry struct {
	ID      string  `json:"id"`
	Label   string  `json:"label"`
	Version string  `json:"version"`
	BinDir  Path    `json:"binDir"`
	Source  *string `json:"source"`
}
type terminalEnvironment struct {
	Shell    string          `json:"shell"`
	CWD      Path            `json:"cwd"`
	Revision string          `json:"revision"`
	Script   string          `json:"script"`
	Entries  []terminalEntry `json:"entries"`
	Warnings []string        `json:"warnings"`
}

func shellQuote(value string) string { return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'" }
func powershellQuote(value string) string {
	var out strings.Builder
	out.WriteByte('\'')
	for _, ch := range value {
		out.WriteRune(ch)
		if strings.ContainsRune("'‘’‚‛", ch) {
			out.WriteRune(ch)
		}
	}
	out.WriteByte('\'')
	return out.String()
}
func terminalScript(dirs []string) string {
	if len(dirs) == 0 {
		return ""
	}
	quoted := make([]string, 0, len(dirs))
	if runtime.GOOS == "windows" {
		for _, dir := range dirs {
			quoted = append(quoted, powershellQuote(dir))
		}
		return "& {\n  $niceDirs = @(\n    " + strings.Join(quoted, ",\n    ") + "\n  )\n" + `  $niceKeys = @($niceDirs | ForEach-Object { $_.Replace('/', '\').TrimEnd('\') })
  $niceRest = @()
  if ($env:PATH) {
    $niceRest = @($env:PATH.Split(';') | Where-Object {
      $niceKeys -notcontains $_.Replace('/', '\').TrimEnd('\')
    })
  }
  $env:PATH = (@($niceDirs) + $niceRest) -join ';'
}`
	}
	for _, dir := range dirs {
		quoted = append(quoted, shellQuote(dir))
	}
	return "PATH=$(\n  nice_result=" + shellQuote(strings.Join(dirs, ":")) + "\n" + `  nice_rest=${PATH-}
  if [ -n "$nice_rest" ]; then
    while :; do
      nice_item=${nice_rest%%:*}
      nice_selected=0
      for nice_dir in ` + strings.Join(quoted, " ") + `; do
        if [ "$nice_item" = "$nice_dir" ]; then nice_selected=1; break; fi
      done
      if [ "$nice_selected" = 0 ]; then nice_result=$nice_result:$nice_item; fi
      if [ "$nice_rest" = "$nice_item" ]; then break; fi
      nice_rest=${nice_rest#*:}
    done
  fi
  printf '%s.' "$nice_result"
)
PATH=${PATH%.}
export PATH`
}
func (c *Core) terminalEnvironment(ctx context.Context, siteID string) (terminalEnvironment, error) {
	out := terminalEnvironment{Shell: "posix", CWD: Path(c.Home), Entries: []terminalEntry{}, Warnings: []string{}}
	if runtime.GOOS == "windows" {
		out.Shell = "powershell"
	}
	selection, err := c.pathSelection(ctx)
	if err != nil {
		return out, err
	}
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return out, err
	}
	required, sources := map[string]string{}, map[string]string{}
	projectRevision := ""
	if siteID != "" {
		p, err := c.openProject(ctx, siteID, false)
		if err != nil {
			return out, err
		}
		defer p.root.Close()
		content, exists, err := envRead(p.root, ".niceenv.json")
		if err != nil {
			return out, err
		}
		view, err := c.runtimeProjectView(ctx, p, content, exists, projectRuntimeFiles(p.root), packages)
		if err != nil {
			return out, err
		}
		required, sources, err = effectiveProjectVersions(view.Versions, view.Detected)
		if err != nil {
			return out, err
		}
		var siteRuntime struct {
			Kind       string `json:"kind"`
			PHPVersion string `json:"phpVersion"`
		}
		if json.Unmarshal([]byte(p.runtime), &siteRuntime) != nil {
			return out, invalid("站点运行时配置无效")
		}
		if siteRuntime.Kind == "php" && required["php"] == "" {
			if siteRuntime.PHPVersion == "" {
				return out, &AppError{Code: "TERMINAL_RUNTIME_UNAVAILABLE", Message: "PHP 站点尚未指定 PHP 版本"}
			}
			required["php"] = siteRuntime.PHPVersion
			sources["php"] = "站点 PHP 版本"
		}
		if err = validateRuntimeVersions(required, packages); err != nil {
			return out, err
		}
		out.CWD = Path(p.root.Name())
		projectRevision = view.Revision
	}
	for id, version := range required {
		selection.Versions[id] = version
	}
	chosen, err := c.pathChosen(ctx, selection, packages)
	if err != nil {
		return out, err
	}
	selected := map[string]Package{}
	for id, version := range selection.Versions {
		if _, ok := chosen[id]; !ok && pathWants(selection, id) {
			out.Warnings = append(out.Warnings, id+" "+version+"：所选版本尚未安装，请重新选择")
		}
	}
	for id, p := range chosen {
		if !pathWants(selection, id) && required[id] == "" {
			continue
		}
		if _, err := runtimeCLI(p); err != nil {
			continue
		}
		dir, err := runtimeDirectory(p)
		if err != nil {
			out.Warnings = append(out.Warnings, runtimeLabel(p)+" "+p.Version+"："+publicError(err).Message)
			continue
		}
		entry := terminalEntry{ID: id, Label: runtimeLabel(p), Version: p.Version, BinDir: Path(dir)}
		if source, ok := sources[id]; ok {
			entry.Source = &source
		}
		out.Entries = append(out.Entries, entry)
		selected[id] = p
	}
	if err = validateJSCLI(selected); err != nil {
		return out, err
	}
	sort.Slice(out.Entries, func(i, j int) bool {
		a, b := out.Entries[i], out.Entries[j]
		if commandPriority(a.ID) != commandPriority(b.ID) {
			return commandPriority(a.ID) < commandPriority(b.ID)
		}
		if (required[a.ID] != "") != (required[b.ID] != "") {
			return required[a.ID] != ""
		}
		return a.ID < b.ID
	})
	sort.Strings(out.Warnings)
	dirs := []string{}
	for _, entry := range out.Entries {
		dirs = append(dirs, string(entry.BinDir))
	}
	out.Script = terminalScript(dirs)
	payload, _ := json.Marshal([]any{out, siteID, projectRevision})
	digest := sha256.Sum256(payload)
	out.Revision = hex.EncodeToString(digest[:])
	return out, nil
}
func (c *Core) terminalCommand(ctx context.Context, data json.RawMessage, open bool) (any, error) {
	in, err := decode[struct {
		SiteID           string `json:"siteId"`
		ExpectedRevision string `json:"expectedRevision"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.siteChanges.TryLock() {
		return nil, serviceBusy()
	}
	defer c.siteChanges.Unlock()
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	environment, err := c.terminalEnvironment(ctx, in.SiteID)
	if err != nil {
		return nil, err
	}
	if !open {
		return environment, nil
	}
	if in.ExpectedRevision == "" || in.ExpectedRevision != environment.Revision {
		return nil, &AppError{Code: "TERMINAL_ENV_CHANGED", Message: "终端环境已改变，请重新预览后再打开"}
	}
	dir := string(environment.CWD)
	if !filepath.IsAbs(dir) || strings.IndexFunc(dir, unicode.IsControl) >= 0 {
		return nil, invalid("终端目录无效")
	}
	info, err := os.Stat(dir)
	if err != nil {
		return nil, err
	}
	if !info.IsDir() {
		return nil, invalid("终端工作目录不是文件夹")
	}
	if err = launchTerminal(ctx, environment); err != nil {
		return nil, err
	}
	return true, nil
}
func (c *Core) registerTerminal() {
	c.handle("terminal_environment", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.terminalCommand(ctx, data, false)
	})
	c.handle("open_terminal", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.terminalCommand(ctx, data, true)
	})
}
