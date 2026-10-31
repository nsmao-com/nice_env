//go:build !windows

package core

import (
	"context"
	"os/exec"
	"runtime"
	"time"
)

func launchTerminal(ctx context.Context, environment terminalEnvironment) error {
	if runtime.GOOS != "darwin" {
		return &AppError{Code: "TERMINAL_UNSUPPORTED", Message: "此平台请打开 Bash / Zsh 并粘贴预览脚本"}
	}
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	script := "cd -- " + shellQuote(string(environment.CWD)) + " || exit\n" + environment.Script + "\nexec /bin/zsh -f"
	command := "/bin/zsh -f -c " + shellQuote(script)
	output, err := exec.CommandContext(ctx, "/usr/bin/osascript", "-e", "on run argv\ntell application \"Terminal\"\ndo script (item 1 of argv)\nactivate\nend tell\nend run", "--", command).CombinedOutput()
	if err != nil {
		return &AppError{Code: "TERMINAL_OPEN_FAILED", Message: "无法打开终端，请检查系统自动化权限", Detail: string(output)}
	}
	return nil
}
