//go:build windows

package process

import (
	"errors"
	"fmt"
	"path/filepath"
	"strings"

	"golang.org/x/sys/windows"
)

// Expand each batch operand once, after cmd's metacharacter/percent parsing.
// CALL is deliberately avoided because it reparses operands and expands % again.
func batchSpec(spec Spec) (Spec, error) {
	ext := strings.ToLower(filepath.Ext(spec.Program))
	if ext != ".bat" && ext != ".cmd" {
		return spec, nil
	}
	system, err := windows.GetSystemDirectory()
	if err != nil {
		return spec, err
	}
	values := append([]string{spec.Program}, spec.Args...)
	env := map[string]string{}
	for key, value := range spec.Environment {
		env[key] = value
	}
	parts := make([]string, len(values))
	for i, value := range values {
		if strings.ContainsAny(value, "\"\r\n\x00") {
			return spec, errors.New("batch argument contains a quote or control character")
		}
		key := fmt.Sprintf("NICEENV_BATCH_ARG_%d", i)
		env[key] = value
		parts[i] = `"!` + key + `!"`
	}
	spec.Program = filepath.Join(system, "cmd.exe")
	spec.Args = nil
	spec.Environment = env
	spec.commandLine = windows.EscapeArg(spec.Program) + ` /D /V:ON /S /C "` + strings.Join(parts, " ") + `"`
	return spec, nil
}
