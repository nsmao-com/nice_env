// niceenv-cli exposes the native registry for automation. Packaged nsbctl and
// nsb-mcp connect to the desktop controller so service ownership survives exit.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"niceenv/internal/core"
	"niceenv/internal/hostfile"
	"niceenv/internal/process"
)

var version = "2.0.8-dev"

func main() {
	if handled, err := process.RunConsoleHelper(os.Args[1:]); handled {
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	if handled, err := hostfile.RunHelper(os.Args[1:]); handled {
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	name := strings.TrimSuffix(strings.ToLower(filepath.Base(os.Args[0])), ".exe")
	if name == "nsbctl" {
		os.Exit(runLegacyCLI(os.Args[1:]))
	}
	if name == "nsb-mcp" {
		if len(os.Args) > 1 {
			fmt.Fprintln(os.Stderr, "用法：nsb-mcp（MCP 标准输入输出服务）")
			os.Exit(2)
		}
		home, err := core.ApplicationHome()
		if err == nil {
			err = core.RunMCP(context.Background(), home, version, os.Stdin, os.Stdout)
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, string(core.MarshalError(err)))
			os.Exit(1)
		}
		return
	}
	command := flag.String("command", "backend_capabilities", "Native command name")
	input := flag.String("args", "{}", "Command arguments as JSON; prefer stdin for sensitive values")
	stdin := flag.Bool("stdin", false, "Read command arguments from stdin")
	batch := flag.Bool("batch", false, "Read newline-delimited command requests; services live until input closes")
	schedulers := flag.Bool("schedulers", false, "Enable scheduled jobs and service recovery for a batch session")
	mcp := flag.Bool("mcp", false, "Serve MCP over stdio using the running desktop instance")
	remote := flag.Bool("remote", false, "Send a command to the running desktop instance")
	dataHome := flag.String("niceenv-data-home", "", "Application data directory")
	flag.Parse()
	if *dataHome != "" {
		if !filepath.IsAbs(*dataHome) {
			fmt.Fprintln(os.Stderr, "data directory must be absolute")
			os.Exit(1)
		}
		if err := os.Setenv("NICEENV_WAILS_HOME", *dataHome); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
	}
	if *mcp {
		home, err := core.ApplicationHome()
		if err == nil {
			err = core.RunMCP(context.Background(), home, version, os.Stdin, os.Stdout)
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, string(core.MarshalError(err)))
			os.Exit(1)
		}
		return
	}
	if *batch {
		if err := runBatch(*schedulers); err != nil {
			fmt.Fprintln(os.Stderr, string(core.MarshalError(err)))
			os.Exit(1)
		}
		return
	}
	if err := run(*command, *input, *stdin, *remote); err != nil {
		fmt.Fprintln(os.Stderr, string(core.MarshalError(err)))
		os.Exit(1)
	}
}

// Batch sessions share one Core, allowing local automation to observe and stop
// the same owned service instance. EOF closes that session and its process trees.
func runBatch(schedulers bool) (resultErr error) {
	home, err := core.ApplicationHome()
	if err != nil {
		return err
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	backend, err := core.Open(ctx, home, version, nil)
	if err != nil {
		return err
	}
	if schedulers {
		backend.StartSchedulers(ctx)
	}
	defer func() {
		quitCtx, quitCancel := context.WithTimeout(context.Background(), 90*time.Second)
		defer quitCancel()
		resultErr = errors.Join(resultErr, backend.PrepareQuit(quitCtx), backend.Close())
	}()
	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 4096), 4*1024*1024)
	encoder := json.NewEncoder(os.Stdout)
	for scanner.Scan() {
		var request struct {
			Command string          `json:"command"`
			Args    json.RawMessage `json:"args"`
		}
		if err = json.Unmarshal(scanner.Bytes(), &request); err != nil {
			return &core.AppError{Code: "INVALID_ARGUMENT", Message: "每行必须为包含 command 和 args 的 JSON"}
		}
		callCtx, callCancel := context.WithTimeout(ctx, 5*time.Minute)
		result, callErr := backend.Invoke(callCtx, request.Command, request.Args)
		callCancel()
		response := map[string]any{"result": result}
		if callErr != nil {
			response = map[string]any{"error": json.RawMessage(core.MarshalError(callErr))}
		}
		if err = encoder.Encode(response); err != nil {
			return err
		}
	}
	return scanner.Err()
}

func run(command, input string, stdin, remote bool) error {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	args := json.RawMessage(input)
	if stdin {
		if err := json.NewDecoder(io.LimitReader(os.Stdin, 4*1024*1024+1)).Decode(&args); err != nil {
			return &core.AppError{Code: "INVALID_ARGUMENT", Message: "输入必须是合法 JSON"}
		}
	}
	if !json.Valid(args) {
		return &core.AppError{Code: "INVALID_ARGUMENT", Message: "参数必须是合法 JSON"}
	}
	home, err := core.ApplicationHome()
	if err != nil {
		return err
	}
	if remote {
		result, err := core.ControlInvoke(ctx, home, command, args)
		if err != nil {
			return err
		}
		return json.NewEncoder(os.Stdout).Encode(result)
	}
	backend, err := core.Open(ctx, home, version, nil)
	if err != nil {
		return err
	}
	result, invokeErr := backend.Invoke(ctx, command, args)
	quitCtx, quitCancel := context.WithTimeout(context.Background(), 90*time.Second)
	quitErr := backend.PrepareQuit(quitCtx)
	quitCancel()
	closeErr := errors.Join(quitErr, backend.Close())
	if invokeErr != nil {
		return invokeErr
	}
	if closeErr != nil {
		return closeErr
	}
	return json.NewEncoder(os.Stdout).Encode(result)
}
