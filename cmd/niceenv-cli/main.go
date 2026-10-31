// niceenv-cli calls the same native Go command registry without starting WebView2,
// schedulers or managed services. It is restricted to the isolated Wails home.
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
	"time"

	"niceenv/internal/core"
	"niceenv/internal/hostfile"
)

func main() {
	if handled, err := hostfile.RunHelper(os.Args[1:]); handled {
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	command := flag.String("command", "backend_capabilities", "Native command name")
	input := flag.String("args", "{}", "Command arguments as JSON; prefer stdin for sensitive values")
	stdin := flag.Bool("stdin", false, "Read command arguments from stdin")
	batch := flag.Bool("batch", false, "Read newline-delimited command requests; services live until input closes")
	flag.Parse()
	if *batch {
		if err := runBatch(); err != nil {
			fmt.Fprintln(os.Stderr, string(core.MarshalError(err)))
			os.Exit(1)
		}
		return
	}
	if err := run(*command, *input, *stdin); err != nil {
		fmt.Fprintln(os.Stderr, string(core.MarshalError(err)))
		os.Exit(1)
	}
}

// Batch sessions share one Core, allowing local automation to observe and stop
// the same owned service instance. EOF closes that session and its process trees.
func runBatch() (resultErr error) {
	home, err := core.DevelopmentHome()
	if err != nil {
		return err
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	backend, err := core.Open(ctx, home, "1.0.4-wails-dev", nil)
	if err != nil {
		return err
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

func run(command, input string, stdin bool) error {
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
	home, err := core.DevelopmentHome()
	if err != nil {
		return err
	}
	backend, err := core.Open(ctx, home, "1.0.4-wails-dev", nil)
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
