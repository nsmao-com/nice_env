//go:build !windows

package core

func systemCurl() (string, error) { return "/usr/bin/curl", nil }
