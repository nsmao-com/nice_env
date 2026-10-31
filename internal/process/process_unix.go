//go:build linux || darwin

package process

import (
	"bufio"
	"context"
	"errors"
	"io"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"syscall"
)

func Shell(command, directory string, stdout, stderr io.Writer) (Spec, error) {
	return Spec{Program: "/bin/sh", Args: []string{"-c", command}, Directory: directory, Stdout: stdout, Stderr: stderr}, nil
}

func start(ctx context.Context, spec Spec) (*Child, error) {
	// Keep the process-group leader alive while the actual command runs. The
	// leader waits for an explicit release from this owner before exiting, so a
	// cancellation never addresses a recycled process-group ID.
	return startSupervised(ctx, spec)
}

func startSupervised(ctx context.Context, spec Spec) (*Child, error) {
	// No shell interpolation: every program argument remains a separate argv.
	// A status pipe carries the exit code, then the supervisor waits until its
	// owner closes the control pipe. All descendants share the supervisor group.
	statusR, statusW, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	controlR, controlW, err := os.Pipe()
	if err != nil {
		statusR.Close()
		statusW.Close()
		return nil, err
	}
	defer statusW.Close()
	defer controlR.Close()
	// A caught (not ignored) TERM is reset to default in the executed command.
	// Keep the leader alive until that command finishes its graceful shutdown.
	cmd := exec.Command("/bin/sh", "-c", `trap ':' TERM
leader=$$
(trap '' TERM; read hold <&4; /bin/kill -KILL -"$leader") 3>&- &
"$@" 3>&- 4<&-
code=$?
printf '%s\n' "$code" >&3
read hold <&4`, "niceenv-supervisor")
	cmd.Args = append(cmd.Args, spec.Program)
	cmd.Args = append(cmd.Args, spec.Args...)
	cmd.Dir = spec.Directory
	cmd.Env = environment(spec.Environment, spec.RemoveEnvironment)
	cmd.Stdin = spec.Stdin
	cmd.Stdout = spec.Stdout
	cmd.Stderr = spec.Stderr
	cmd.ExtraFiles = []*os.File{statusW, controlR}
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err = cmd.Start(); err != nil {
		statusR.Close()
		controlW.Close()
		return nil, err
	}
	p := &Child{PID: cmd.Process.Pid, done: make(chan struct{})}
	p.owns = func(pid int) (bool, error) {
		group, err := syscall.Getpgid(pid)
		return group == p.PID, err
	}
	p.graceful = func() error { return syscall.Kill(-p.PID, syscall.SIGTERM) }
	p.terminate = func() error {
		err := syscall.Kill(-p.PID, syscall.SIGKILL)
		if errors.Is(err, syscall.ESRCH) {
			return nil
		}
		return err
	}
	go func() {
		select {
		case <-ctx.Done():
			_ = p.Stop()
		case <-p.done:
		}
	}()
	go func() {
		defer statusR.Close()
		defer controlW.Close()
		line, readErr := bufio.NewReader(io.LimitReader(statusR, 32)).ReadString('\n')
		code, parseErr := strconv.Atoi(strings.TrimSpace(line))
		p.mu.Lock()
		stopErr := p.terminate()
		waitErr := cmd.Wait()
		if exit := new(exec.ExitError); errors.As(waitErr, &exit) {
			waitErr = nil
		}
		p.result = Result{Code: code, Err: errors.Join(readErr, parseErr, stopErr, waitErr)}
		close(p.done)
		p.mu.Unlock()
	}()
	return p, nil
}
