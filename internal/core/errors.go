package core

import (
	"context"
	"encoding/json"
	"errors"
	"os"
)

type AppError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
	Hint    string `json:"hint,omitempty"`
	Detail  string `json:"detail,omitempty"`
}

func (e *AppError) Error() string { return e.Message }

func MarshalError(err error) []byte {
	var appError *AppError
	if !errors.As(err, &appError) {
		var pathError *os.PathError
		// Internal errors may contain credentials or SQL. Keep them out of IPC.
		appError = &AppError{Code: "INTERNAL", Message: "操作失败，请检查配置和文件权限"}
		switch {
		case errors.Is(err, context.Canceled):
			appError = &AppError{Code: "CANCELLED", Message: "操作已取消"}
		case errors.Is(err, context.DeadlineExceeded):
			appError = &AppError{Code: "TIMEOUT", Message: "操作超时，请稍后重试"}
		case errors.As(err, &pathError):
			appError = &AppError{Code: "IO_FAILED", Message: "文件操作失败", Hint: "请确认文件存在且当前用户具有所需权限。", Detail: DisplayPath(pathError.Path) + ": " + pathError.Err.Error()}
		}
	}
	data, _ := json.Marshal(appError)
	return data
}

func invalid(message string) error { return &AppError{Code: "INVALID_ARGUMENT", Message: message} }
