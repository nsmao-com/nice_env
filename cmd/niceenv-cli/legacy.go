package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
	"unicode"

	"niceenv/internal/core"
)

const cliHelp = `nsbctl — NiceEnv Go CLI
用法：nsbctl <命令>
  help | --version
  status [--json] | sites [--json] | packages [--json]
  start|stop|restart <service> | kill <service> --yes
  start-all [--json] | stop-all [--json]
  open <site> | logs <service|site:id|site-error:id> [1–5000]
  diagnose <port> | pin <php@x.y.z> <directory>
服务命令连接现有 NiceEnv；尚未运行时自动启动托盘应用，命令退出后服务继续运行。
`

func runLegacyCLI(args []string) int {
	fail := func(code int, err error) int { fmt.Fprintln(os.Stderr, "nsbctl:", err); return code }
	if len(args) == 0 {
		fmt.Print(cliHelp)
		return 2
	}
	cmd := args[0]
	if len(args) == 2 && (args[1] == "--help" || args[1] == "-h") {
		fmt.Print(cliHelp)
		return 0
	}
	positions := []string{}
	jsonOutput, confirmed := false, false
	for _, arg := range args[1:] {
		switch {
		case arg == "--json" && !jsonOutput && (cmd == "status" || cmd == "sites" || cmd == "packages" || cmd == "start-all" || cmd == "stop-all"):
			jsonOutput = true
		case arg == "--yes" && !confirmed && cmd == "kill":
			confirmed = true
		case strings.HasPrefix(arg, "-"):
			return fail(2, fmt.Errorf("不支持或重复的参数 %s", arg))
		default:
			positions = append(positions, arg)
		}
	}
	want := 0
	switch cmd {
	case "help", "--help", "-h", "version", "--version", "-V", "status", "sites", "packages", "start-all", "stop-all":
	case "start", "stop", "restart", "kill", "open", "diagnose":
		want = 1
	case "pin":
		want = 2
	case "logs":
		want = 1
		if len(positions) == 2 {
			want = 2
		}
	default:
		return fail(2, fmt.Errorf("未知命令 %s", cmd))
	}
	if len(positions) != want {
		return fail(2, fmt.Errorf("%s 需要 %d 个位置参数", cmd, want))
	}
	for _, value := range positions {
		if value == "" || strings.IndexFunc(value, unicode.IsControl) >= 0 {
			return fail(2, errors.New("参数为空或含控制字符"))
		}
	}
	switch cmd {
	case "help", "--help", "-h":
		fmt.Print(cliHelp)
		return 0
	case "version", "--version", "-V":
		fmt.Println("nsbctl", version)
		return 0
	case "pin":
		if !strings.HasPrefix(positions[0], "php@") {
			return fail(2, errors.New("目前仅支持 php@x.y.z"))
		}
		if err := core.PinProjectPHP(positions[1], strings.TrimPrefix(positions[0], "php@")); err != nil {
			return fail(1, err)
		}
		fmt.Println("pinned", positions[0])
		return 0
	}
	if cmd == "kill" && !confirmed {
		return fail(2, errors.New("强制停止可能中断数据库写入；请使用 kill <service> --yes"))
	}
	if cmd == "start" || cmd == "stop" || cmd == "restart" || cmd == "kill" || cmd == "logs" {
		if len(positions[0]) > 512 || strings.IndexFunc(positions[0], unicode.IsSpace) >= 0 {
			return fail(2, errors.New("服务标识无效"))
		}
	}
	lines, port := 50, 0
	if cmd == "logs" && len(positions) == 2 {
		var err error
		lines, err = strconv.Atoi(positions[1])
		if err != nil || lines < 1 || lines > 5000 {
			return fail(2, errors.New("日志行数必须为 1–5000"))
		}
	}
	if cmd == "diagnose" {
		var err error
		port, err = strconv.Atoi(positions[0])
		if err != nil || port < 1 || port > 65535 {
			return fail(2, errors.New("端口必须为 1–65535"))
		}
		result, err := core.DiagnosePort(context.Background(), port)
		if err != nil {
			return fail(1, err)
		}
		if err = json.NewEncoder(os.Stdout).Encode(result); err != nil {
			return fail(1, err)
		}
		return 0
	}
	home, err := core.ApplicationHome()
	if err != nil {
		return fail(1, err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Minute)
	defer cancel()
	if err = core.EnsureDesktopController(ctx, home); err != nil {
		return fail(1, err)
	}
	call := func(name string, args any) (json.RawMessage, error) {
		body, err := json.Marshal(args)
		if err != nil {
			return nil, err
		}
		return core.ControlInvoke(ctx, home, name, body)
	}
	printJSON := func(body json.RawMessage) error {
		var pretty bytes.Buffer
		if err := json.Indent(&pretty, body, "", "  "); err != nil {
			return err
		}
		_, err := fmt.Fprintln(os.Stdout, pretty.String())
		return err
	}
	var result json.RawMessage
	switch cmd {
	case "status", "sites", "packages":
		command := map[string]string{"status": "list_service_status", "sites": "list_sites", "packages": "list_packages"}[cmd]
		result, err = call(command, map[string]any{})
		if err != nil {
			break
		}
		var rows []map[string]any
		if err = json.Unmarshal(result, &rows); err != nil {
			break
		}
		if cmd == "packages" {
			installed := []map[string]any{}
			for _, row := range rows {
				if row["install"] != nil {
					installed = append(installed, row)
				}
			}
			rows = installed
			result, err = json.Marshal(rows)
		}
		if !jsonOutput {
			for _, row := range rows {
				switch cmd {
				case "status":
					fmt.Printf("%-24v %-10v port=%v pids=%v\n", row["id"], row["state"], row["port"], row["pids"])
				case "sites":
					fmt.Printf("%-24v %-10v %v\n", row["name"], row["status"], row["accessUrl"])
				case "packages":
					fmt.Printf("%-24v %v\n", row["id"], row["version"])
				}
			}
			return 0
		}
	case "start", "stop", "restart", "kill":
		id := positions[0]
		if len(id) > 512 || strings.IndexFunc(id, unicode.IsSpace) >= 0 {
			return fail(2, errors.New("服务标识无效"))
		}
		preview, err := call("service_stop_preview", map[string]string{"id": id})
		if err != nil {
			return fail(1, err)
		}
		var target struct {
			Service struct {
				Version string `json:"version"`
			} `json:"service"`
			Revision string `json:"revision"`
		}
		if err = json.Unmarshal(preview, &target); err != nil {
			return fail(1, err)
		}
		command := map[string]string{"start": "start_service", "stop": "stop_service", "restart": "restart_service", "kill": "force_stop_service"}[cmd]
		_, err = call(command, map[string]string{"id": id, "expectedVersion": target.Service.Version, "revision": target.Revision})
		if err != nil {
			return fail(1, err)
		}
		fmt.Println("ok", id)
		return 0
	case "start-all", "stop-all":
		body, callErr := call("list_service_status", map[string]any{})
		if callErr != nil {
			return fail(1, callErr)
		}
		var rows []struct {
			ID      string `json:"id"`
			Version string `json:"version"`
		}
		if err = json.Unmarshal(body, &rows); err != nil {
			break
		}
		targets := []map[string]string{}
		for _, row := range rows {
			preview, err := call("service_stop_preview", map[string]string{"id": row.ID})
			if err != nil {
				return fail(1, err)
			}
			var p struct {
				Revision string `json:"revision"`
			}
			if err = json.Unmarshal(preview, &p); err != nil {
				return fail(1, err)
			}
			targets = append(targets, map[string]string{"id": row.ID, "version": row.Version, "revision": p.Revision})
		}
		if len(targets) == 0 {
			result = json.RawMessage(`{"succeeded":[],"already":[],"failed":[]}`)
		} else {
			command := "bulk_start"
			if cmd == "stop-all" {
				command = "bulk_stop"
			}
			result, err = call(command, map[string]any{"targets": targets})
		}
		if err != nil {
			break
		}
		var report struct {
			Failed []json.RawMessage `json:"failed"`
		}
		if err = json.Unmarshal(result, &report); err != nil {
			break
		}
		if err = printJSON(result); err != nil {
			return fail(1, err)
		}
		if len(report.Failed) > 0 {
			return 1
		}
		return 0
	case "logs":
		result, err = call("tail_logs", map[string]any{"id": positions[0], "lines": lines})
		if err != nil {
			break
		}
		var logs []struct {
			Line string `json:"line"`
		}
		if err = json.Unmarshal(result, &logs); err != nil {
			break
		}
		for _, line := range logs {
			fmt.Println(line.Line)
		}
		return 0
	case "diagnose":
		result, err = call("diagnose_port", map[string]int{"port": port})
	case "open":
		body, callErr := call("list_sites", map[string]any{})
		if callErr != nil {
			return fail(1, callErr)
		}
		var sites []struct {
			ID   string `json:"id"`
			Name string `json:"name"`
		}
		if err = json.Unmarshal(body, &sites); err != nil {
			break
		}
		id := ""
		for _, site := range sites {
			if site.Name == positions[0] || site.ID == positions[0] {
				if id != "" {
					return fail(1, errors.New("站点名有歧义，请使用站点 ID"))
				}
				id = site.ID
			}
		}
		if id == "" {
			return fail(1, errors.New("站点不存在"))
		}
		body, err = call("site_access_url", map[string]string{"id": id})
		if err != nil {
			break
		}
		var address string
		if err = json.Unmarshal(body, &address); err != nil {
			break
		}
		u, e := url.Parse(address)
		if e != nil || u.Hostname() == "" || u.User != nil || (u.Scheme != "http" && u.Scheme != "https") || strings.ContainsAny(address, "\r\n\x00") {
			return fail(1, errors.New("站点地址无效"))
		}
		var browser *exec.Cmd
		switch runtime.GOOS {
		case "windows":
			browser = exec.Command(filepath.Join(os.Getenv("SystemRoot"), "System32", "rundll32.exe"), "url.dll,FileProtocolHandler", address)
		case "darwin":
			browser = exec.Command("/usr/bin/open", address)
		default:
			browser = exec.Command("xdg-open", address)
		}
		if err = browser.Run(); err != nil {
			break
		}
		fmt.Println("opened", address)
		return 0
	}
	if err != nil {
		return fail(1, err)
	}
	if err = printJSON(result); err != nil {
		return fail(1, err)
	}
	return 0
}
