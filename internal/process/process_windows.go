//go:build windows

package process

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"unsafe"

	"golang.org/x/sys/windows"
)

func Shell(command, directory string, stdout, stderr io.Writer) (Spec, error) {
	system, err := windows.GetSystemDirectory()
	if err != nil {
		return Spec{}, err
	}
	program := filepath.Join(system, "cmd.exe")
	// The inner shell inherits UTF-8 after chcp. Delayed expansion in the outer
	// shell inserts the command once, preserving %, ! and ^ in user commands.
	return Spec{Program: program, Directory: directory, Stdout: stdout, Stderr: stderr,
		Environment: map[string]string{"NICEENV_CRON_COMMAND": command},
		commandLine: windows.EscapeArg(program) + ` /D /V:ON /S /C "chcp 65001 >nul && cmd /D /V:OFF /S /C "!NICEENV_CRON_COMMAND!""`,
	}, nil
}

func start(ctx context.Context, spec Spec) (*Child, error) {
	job, err := windows.CreateJobObject(nil, nil)
	if err != nil {
		return nil, err
	}
	owned := false
	defer func() {
		if !owned {
			windows.CloseHandle(job)
		}
	}()
	limits := windows.JOBOBJECT_EXTENDED_LIMIT_INFORMATION{}
	limits.BasicLimitInformation.LimitFlags = windows.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
	if _, err = windows.SetInformationJobObject(job, windows.JobObjectExtendedLimitInformation, uintptr(unsafe.Pointer(&limits)), uint32(unsafe.Sizeof(limits))); err != nil {
		return nil, err
	}
	attributes, err := windows.NewProcThreadAttributeList(2)
	if err != nil {
		return nil, err
	}
	defer attributes.Delete()
	// PROC_THREAD_ATTRIBUTE_JOB_LIST joins the job during CreateProcess, before
	// the first instruction. Children cannot escape through an attach-after-start race.
	const jobListAttribute = 0x0002000d
	if err = attributes.Update(jobListAttribute, unsafe.Pointer(&job), unsafe.Sizeof(job)); err != nil {
		return nil, err
	}
	input := spec.Stdin
	if input == nil {
		input, err = os.Open(os.DevNull)
		if err != nil {
			return nil, err
		}
		defer input.Close()
	}
	outRead, outWrite, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	defer outWrite.Close()
	errRead, errWrite, err := os.Pipe()
	if err != nil {
		outRead.Close()
		return nil, err
	}
	defer errWrite.Close()
	launched := false
	defer func() {
		if !launched {
			outRead.Close()
			errRead.Close()
		}
	}()
	handles := make([]windows.Handle, 3)
	defer func() {
		for _, handle := range handles {
			if handle != 0 {
				windows.CloseHandle(handle)
			}
		}
	}()
	for i, file := range []*os.File{input, outWrite, errWrite} {
		if err = windows.DuplicateHandle(windows.CurrentProcess(), windows.Handle(file.Fd()), windows.CurrentProcess(), &handles[i], 0, true, windows.DUPLICATE_SAME_ACCESS); err != nil {
			return nil, err
		}
	}
	if err = attributes.Update(windows.PROC_THREAD_ATTRIBUTE_HANDLE_LIST, unsafe.Pointer(&handles[0]), uintptr(len(handles))*unsafe.Sizeof(handles[0])); err != nil {
		return nil, err
	}
	si := windows.StartupInfoEx{StartupInfo: windows.StartupInfo{Cb: uint32(unsafe.Sizeof(windows.StartupInfoEx{})), Flags: windows.STARTF_USESTDHANDLES | windows.STARTF_USESHOWWINDOW, ShowWindow: windows.SW_HIDE, StdInput: handles[0], StdOutput: handles[1], StdErr: handles[2]}, ProcThreadAttributeList: attributes.List()}
	program, err := windows.UTF16PtrFromString(spec.Program)
	if err != nil {
		return nil, err
	}
	line := spec.commandLine
	if line == "" {
		line = windows.ComposeCommandLine(append([]string{spec.Program}, spec.Args...))
	}
	command, err := windows.UTF16PtrFromString(line)
	if err != nil {
		return nil, err
	}
	var directory *uint16
	if spec.Directory != "" {
		directory, err = windows.UTF16PtrFromString(spec.Directory)
		if err != nil {
			return nil, err
		}
	}
	env := environment(spec.Environment, spec.RemoveEnvironment)
	for _, entry := range env {
		if strings.ContainsRune(entry, 0) {
			return nil, errors.New("invalid environment variable")
		}
	}
	block := []uint16{}
	for _, entry := range env {
		encoded, err := windows.UTF16FromString(entry)
		if err != nil {
			return nil, err
		}
		block = append(block, encoded...)
	}
	block = append(block, 0)
	if len(block) == 1 {
		block = append(block, 0)
	}
	var info windows.ProcessInformation
	if err = ctx.Err(); err != nil {
		return nil, err
	}
	err = windows.CreateProcess(program, command, nil, nil, true, windows.CREATE_NO_WINDOW|windows.CREATE_UNICODE_ENVIRONMENT|windows.EXTENDED_STARTUPINFO_PRESENT, &block[0], directory, &si.StartupInfo, &info)
	runtime.KeepAlive(block)
	runtime.KeepAlive(handles)
	runtime.KeepAlive(job)
	if err != nil {
		return nil, err
	}
	windows.CloseHandle(info.Thread)
	owned = true
	launched = true
	p := &Child{PID: int(info.ProcessId), done: make(chan struct{})}
	p.terminate = func() error { return windows.TerminateJobObject(job, 1) }
	p.owns = func(pid int) (bool, error) {
		handle, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, uint32(pid))
		if err != nil {
			return false, err
		}
		defer windows.CloseHandle(handle)
		var member int32
		result, _, callErr := windows.NewLazySystemDLL("kernel32.dll").NewProc("IsProcessInJob").Call(uintptr(handle), uintptr(job), uintptr(unsafe.Pointer(&member)))
		if result == 0 {
			return false, callErr
		}
		return member != 0, nil
	}
	// Closing our writers before returning ensures EOF once the owned tree exits.
	outWrite.Close()
	errWrite.Close()
	for i, handle := range handles {
		windows.CloseHandle(handle)
		handles[i] = 0
	}
	var readers sync.WaitGroup
	var stdoutErr, stderrErr error
	readers.Add(2)
	go func() { defer readers.Done(); defer outRead.Close(); _, stdoutErr = io.Copy(spec.Stdout, outRead) }()
	go func() { defer readers.Done(); defer errRead.Close(); _, stderrErr = io.Copy(spec.Stderr, errRead) }()
	go func() {
		select {
		case <-ctx.Done():
			_ = p.Stop()
		case <-p.done:
		}
	}()
	go func() {
		_, waitErr := windows.WaitForSingleObject(info.Process, windows.INFINITE)
		var code uint32
		codeErr := windows.GetExitCodeProcess(info.Process, &code)
		p.mu.Lock()
		// The root exited; terminate any descendants before releasing handles.
		treeErr := windows.TerminateJobObject(job, code)
		windows.CloseHandle(job)
		windows.CloseHandle(info.Process)
		readers.Wait()
		p.result = Result{Code: int(code), Err: errors.Join(waitErr, codeErr, treeErr, stdoutErr, stderrErr)}
		close(p.done)
		p.mu.Unlock()
	}()
	return p, nil
}
