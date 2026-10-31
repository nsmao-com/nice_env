//go:build !windows

package core

func systemHTTPProxy() (string, string) { return "", "" }
