//go:build linux || darwin

package process

func RunConsoleHelper([]string) (bool, error) { return false, nil }
