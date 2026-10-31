package desktop

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"niceenv/internal/core"
	"niceenv/internal/process"
)

func (b *Bridge) restart(ctx context.Context, target *string) error {
	if !b.quitting.CompareAndSwap(false, true) {
		return &core.AppError{Code: "QUIT_PENDING", Message: "应用正在停止服务或重启"}
	}
	committed := false
	defer func() {
		if !committed {
			b.quitting.Store(false)
		}
	}()
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	home := b.home
	rollback := func() error { return nil }
	if target != nil {
		home = core.DisplayPath(*target)
		rollback, err = b.backend.PrepareDataDirectoryActivation(ctx, home)
		if err != nil {
			return err
		}
	} else {
		pending, err := b.backend.Invoke(ctx, "pending_data_dir_migration", nil)
		if err != nil {
			return err
		}
		if pending != nil {
			return &core.AppError{Code: "DATA_DIR_PENDING", Message: "请先切换到已复制的数据目录，或取消迁移"}
		}
		rollback, err = b.backend.PrepareRestart(ctx)
		if err != nil {
			return err
		}
	}
	args := core.RestartArguments(os.Args[1:], home, os.Getpid())
	args = append(args, "--restart-supervisor", "--restart-source", b.home)
	command := exec.Command(exe, args...)
	command.Dir = filepath.Dir(exe)
	command.Env = []string{}
	for _, value := range os.Environ() {
		key, _, _ := strings.Cut(value, "=")
		if !strings.EqualFold(key, "NICEENV_WAILS_HOME") {
			command.Env = append(command.Env, value)
		}
	}
	command.Env = append(command.Env, "NICEENV_WAILS_HOME="+home)
	configureRestartCommand(command)
	output, err := command.StdoutPipe()
	if err != nil {
		return errors.Join(err, rollback())
	}
	if err = command.Start(); err != nil {
		return errors.Join(err, rollback())
	}
	started := make(chan error, 1)
	go func() {
		line, err := bufio.NewReader(io.LimitReader(output, 1024)).ReadString('\n')
		if err == nil && line != "ready\n" {
			err = errors.New("重启监护进程未就绪")
		}
		started <- err
	}()
	select {
	case err = <-started:
	case <-time.After(10 * time.Second):
		err = errors.New("等待重启监护进程超时")
	case <-ctx.Done():
		err = ctx.Err()
	}
	if err != nil {
		_ = command.Process.Kill()
		_ = command.Wait()
		return errors.Join(err, rollback())
	}
	go command.Wait()
	committed = true
	b.quitReady.Store(true)
	b.app.Quit()
	return nil
}

// RunRestartSupervisor stays outside Wails' single-instance and WebView lifetime.
// The source is retained until the replacement frontend confirms it can use Core.
func RunRestartSupervisor(home, source string, parent int) error {
	if parent <= 0 || !filepath.IsAbs(home) || !filepath.IsAbs(source) {
		return errors.New("重启参数无效")
	}
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer listener.Close()
	ticket := make([]byte, 32)
	if _, err = rand.Read(ticket); err != nil {
		return err
	}
	token := hex.EncodeToString(ticket)
	startupOutput := process.NewLimitedBuffer(32 * 1024)
	if _, err = fmt.Fprintln(os.Stdout, "ready"); err != nil {
		return err
	}
	waitCtx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	err = core.WaitForRestartParent(waitCtx, parent)
	cancel()
	if err != nil {
		return err
	}
	launch := func(path string, monitored bool) (*exec.Cmd, error) {
		cmd := exec.Command(exe, core.RestartArguments(os.Args[1:], path, 0)...)
		cmd.Dir = filepath.Dir(exe)
		if monitored {
			cmd.Stderr = startupOutput
		}
		for _, item := range os.Environ() {
			key, _, _ := strings.Cut(item, "=")
			if !strings.EqualFold(key, "NICEENV_WAILS_HOME") && !strings.HasPrefix(strings.ToUpper(key), "NICEENV_RESTART_") {
				cmd.Env = append(cmd.Env, item)
			}
		}
		cmd.Env = append(cmd.Env, "NICEENV_WAILS_HOME="+path)
		if monitored {
			cmd.Env = append(cmd.Env, "NICEENV_RESTART_ADDRESS="+listener.Addr().String(), "NICEENV_RESTART_TOKEN="+token)
		}
		configureRestartCommand(cmd)
		return cmd, cmd.Start()
	}
	child, startErr := launch(home, true)
	if startErr == nil {
		exited := make(chan error, 1)
		go func() { exited <- child.Wait() }()
		ready := make(chan struct{}, 1)
		var state sync.Mutex
		committed, aborted := false, false
		go func() {
			for {
				conn, err := listener.Accept()
				if err != nil {
					return
				}
				_ = conn.SetDeadline(time.Now().Add(3 * time.Second))
				line, err := bufio.NewReader(io.LimitReader(conn, 256)).ReadString('\n')
				if err == nil && line == token+"\n" {
					// Commit before replying: losing the acknowledgement must never
					// trigger rollback while the new instance may be serving requests.
					state.Lock()
					if aborted {
						state.Unlock()
						conn.Close()
						return
					}
					committed = true
					state.Unlock()
					_, _ = io.WriteString(conn, "ready\n")
					conn.Close()
					ready <- struct{}{}
					return
				}
				conn.Close()
			}
		}()
		select {
		case <-ready:
			return nil
		case startErr = <-exited:
			if startErr == nil {
				startErr = errors.New("新窗口就绪前已退出")
			}
		case <-time.After(30 * time.Minute):
			// Profile copying can be large. Stop only this unreleased replacement.
			state.Lock()
			if committed {
				state.Unlock()
				<-ready
				return nil
			}
			aborted = true
			state.Unlock()
			_ = listener.Close()
			_ = child.Process.Kill()
			<-exited
			startErr = errors.New("新窗口未能完成启动")
		}
		state.Lock()
		if committed {
			state.Unlock()
			<-ready
			return nil
		}
		aborted = true
		state.Unlock()
		_ = listener.Close()
	}
	if home != source {
		if err = core.RollbackDataDirectoryActivation(source, home); err != nil {
			return errors.Join(startErr, err)
		}
	}
	message := fmt.Sprintf("重启未完成，已恢复原目录。\n%v\n%s", startErr, startupOutput.String())
	if file, logErr := os.OpenFile(filepath.Join(source, ".niceenv-restart-error.log"), os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0600); logErr == nil {
		if process.PrivateFile(file.Name()) == nil {
			_, _ = file.WriteString(message)
		}
		_ = file.Close()
	}
	fallback, err := launch(source, false)
	if err != nil {
		return errors.Join(startErr, err)
	}
	return fallback.Process.Release()
}

func notifyRestartReady() error {
	address, token := os.Getenv("NICEENV_RESTART_ADDRESS"), os.Getenv("NICEENV_RESTART_TOKEN")
	if address == "" && token == "" {
		return nil
	}
	host, _, err := net.SplitHostPort(address)
	if err != nil || host != "127.0.0.1" || len(token) != 64 {
		return errors.New("重启确认参数无效")
	}
	conn, err := net.DialTimeout("tcp4", address, 5*time.Second)
	if err != nil {
		return err
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(5 * time.Second))
	if _, err = io.WriteString(conn, token+"\n"); err != nil {
		return err
	}
	line, err := bufio.NewReader(io.LimitReader(conn, 64)).ReadString('\n')
	if err != nil {
		return err
	}
	if line != "ready\n" {
		return errors.New("重启确认失败")
	}
	_ = os.Unsetenv("NICEENV_RESTART_ADDRESS")
	_ = os.Unsetenv("NICEENV_RESTART_TOKEN")
	return nil
}
