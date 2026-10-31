//go:build !windows

package process

import "os"

func PrivateFile(path string) error { return os.Chmod(path, 0600) }
