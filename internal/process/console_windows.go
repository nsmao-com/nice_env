//go:build windows

package process

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

const consoleHelperFlag = "--niceenv-console-stop"
const consoleLaunchFlag = "--niceenv-console-launch"

func consoleProcessList(pids []uint32) (int, error) {
	n, _, err := windows.NewLazySystemDLL("kernel32.dll").NewProc("GetConsoleProcessList").Call(uintptr(unsafe.Pointer(&pids[0])), uintptr(len(pids)))
	if n == 0 {
		return 0, err
	}
	if n > uintptr(len(pids)) {
		return 0, errors.New("console process list exceeds limit")
	}
	return int(n), nil
}

func processCreation(handle windows.Handle) (uint64, error) {
	var creation, exit, kernel, user windows.Filetime
	err := windows.GetProcessTimes(handle, &creation, &exit, &kernel, &user)
	return uint64(creation.HighDateTime)<<32 | uint64(creation.LowDateTime), err
}

func requestConsoleStop(handle windows.Handle, pid uint32, interrupt bool) error {
	// The owning Child holds its mutex and original process handle throughout
	// the helper call. The PID cannot be recycled while that handle is open.
	creation, err := processCreation(handle)
	if err != nil {
		return err
	}
	executable, err := os.Executable()
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	args := []string{consoleHelperFlag, strconv.FormatUint(uint64(pid), 10), strconv.FormatUint(creation, 10)}
	if interrupt {
		args = append(args, "interrupt")
	}
	cmd := exec.CommandContext(ctx, executable, args...)
	cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CreationFlags: windows.CREATE_NO_WINDOW}
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("native console stop: %w", err)
	}
	return nil
}

// RunConsoleHelper runs before app/CLI initialization. Attaching a console in a
// separate process avoids changing Wails, its terminal, or any other service.
func RunConsoleHelper(args []string) (bool, error) {
	if len(args) > 0 && args[0] == consoleLaunchFlag {
		if len(args) < 2 || !filepath.IsAbs(args[1]) {
			return true, errors.New("invalid native console launch")
		}
		// GUI/automation hosts may ignore Ctrl+C. Windows propagates that bit to
		// children even in a new console. Reset it inside this isolated launcher,
		// never in Wails or the user's terminal, before starting the native server.
		interrupts := make(chan os.Signal, 1)
		signal.Notify(interrupts, os.Interrupt)
		defer signal.Stop(interrupts)
		if ok, _, err := windows.NewLazySystemDLL("kernel32.dll").NewProc("SetConsoleCtrlHandler").Call(0, 0); ok == 0 {
			return true, err
		}
		spec, err := batchSpec(Spec{Program: args[1], Args: args[2:]})
		if err != nil {
			return true, err
		}
		cmd := exec.Command(spec.Program, spec.Args...)
		cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
		cmd.Env = environment(spec.Environment, nil)
		cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CmdLine: spec.commandLine}
		// Both processes inherit the same job and private console. Keep the job
		// root alive until the server has drained its own shutdown handlers.
		return true, cmd.Run()
	}
	if len(args) == 0 || args[0] != consoleHelperFlag {
		return false, nil
	}
	if len(args) != 3 && !(len(args) == 4 && args[3] == "interrupt") {
		return true, errors.New("invalid console helper arguments")
	}
	pid, err := strconv.ParseUint(args[1], 10, 32)
	if err != nil || pid == 0 {
		return true, errors.New("invalid console target")
	}
	expected, err := strconv.ParseUint(args[2], 10, 64)
	if err != nil || expected == 0 {
		return true, errors.New("invalid console target identity")
	}
	handle, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION|windows.SYNCHRONIZE, false, uint32(pid))
	if err != nil {
		return true, err
	}
	defer windows.CloseHandle(handle)
	creation, err := processCreation(handle)
	if err != nil {
		return true, err
	}
	if creation != expected {
		return true, errors.New("console target changed")
	}
	if state, err := windows.WaitForSingleObject(handle, 0); err != nil {
		return true, err
	} else if state == windows.WAIT_OBJECT_0 {
		return true, nil
	}
	kernel := windows.NewLazySystemDLL("kernel32.dll")
	// CREATE_NO_WINDOW provides no console; FreeConsole also handles invocation
	// from a terminal without allowing a signal to reach that terminal.
	kernel.NewProc("FreeConsole").Call()
	if ok, _, err := kernel.NewProc("AttachConsole").Call(uintptr(pid)); ok == 0 {
		return true, err
	}
	defer kernel.NewProc("FreeConsole").Call()
	// This console was created exclusively for the owned service. Reject a
	// console whose process list does not contain the pinned target.
	pids := make([]uint32, 1024)
	n, err := consoleProcessList(pids)
	if err != nil {
		return true, err
	}
	found := false
	for _, id := range pids[:n] {
		if uint64(id) == pid {
			found = true
		}
	}
	if !found {
		return true, errors.New("console target is no longer attached")
	}
	handler := syscall.NewCallback(func(uint32) uintptr { return 1 })
	if ok, _, err := kernel.NewProc("SetConsoleCtrlHandler").Call(handler, 1); ok == 0 {
		return true, err
	}
	signal := uintptr(windows.CTRL_BREAK_EVENT)
	if len(args) == 4 {
		signal = windows.CTRL_C_EVENT
	}
	// Select the event implemented by the native service. Ctrl+Break is not a
	// substitute for Ctrl+C: a default handler can otherwise terminate storage.
	if ok, _, err := kernel.NewProc("GenerateConsoleCtrlEvent").Call(signal, 0); ok == 0 {
		return true, err
	}
	// Delivery is asynchronous; keep the helper attached briefly while Windows
	// dispatches the event. The owner separately waits for service exit.
	time.Sleep(200 * time.Millisecond)
	return true, nil
}
