//go:build !windows

package core

import (
	"context"
	"net"
	"os/exec"
	"runtime"
	"strings"
	"time"

	"niceenv/internal/process"
)

func networkOutput(ctx context.Context, program string, args ...string) (string, error) {
	path, err := exec.LookPath(program)
	if err != nil {
		return "", &AppError{Code: "DNS_SYSTEM_TOOL_MISSING", Message: "未找到系统网络管理工具：" + program}
	}
	ctx, cancel := context.WithTimeout(ctx, 45*time.Second)
	defer cancel()
	out := process.NewLimitedBuffer(1024 * 1024)
	p, err := process.Start(ctx, process.Spec{Program: path, Args: args, Environment: map[string]string{"LC_ALL": "C"}, Stdout: out, Stderr: out})
	if err != nil {
		return "", err
	}
	result := p.Wait()
	if result.Code != 0 || result.Err != nil {
		return "", &AppError{Code: "DNS_SYSTEM_COMMAND", Message: "系统网络配置操作失败", Detail: out.String()}
	}
	return strings.TrimSpace(out.String()), nil
}
func dnsInterfaces(ctx context.Context) ([]string, error) {
	if runtime.GOOS == "darwin" {
		text, err := networkOutput(ctx, "networksetup", "-listallnetworkservices")
		if err != nil {
			return nil, err
		}
		result := []string{}
		for _, name := range strings.Split(text, "\n")[1:] {
			name = strings.TrimSpace(name)
			if name != "" && !strings.HasPrefix(name, "*") {
				result = append(result, name)
			}
		}
		return result, nil
	}
	text, err := networkOutput(ctx, "nmcli", "-t", "--escape", "no", "-f", "DEVICE,STATE", "device", "status")
	if err != nil {
		return nil, err
	}
	result := []string{}
	for _, line := range strings.Split(text, "\n") {
		name, state, ok := strings.Cut(line, ":")
		if ok && state == "connected" {
			result = append(result, name)
		}
	}
	return result, nil
}
func dnsRead(ctx context.Context, name string) (dnsConfiguration, error) {
	d := dnsConfiguration{Servers: []string{}}
	if err := dnsName(name); err != nil {
		return d, err
	}
	if runtime.GOOS == "darwin" {
		text, err := networkOutput(ctx, "networksetup", "-getdnsservers", name)
		if err != nil {
			return d, err
		}
		d.InterfaceID = name
		d.Automatic = strings.HasPrefix(text, "There aren't any DNS Servers set on ")
		if !d.Automatic {
			d.Servers = strings.Fields(text)
		}
	} else {
		id, err := networkOutput(ctx, "nmcli", "-g", "GENERAL.CON-UUID", "device", "show", name)
		if err != nil {
			return d, err
		}
		if id == "" || id == "--" {
			return d, invalid("网络接口没有活动连接")
		}
		d.InterfaceID = id
		auto, err := networkOutput(ctx, "nmcli", "-g", "ipv4.ignore-auto-dns", "connection", "show", "uuid", id)
		if err != nil {
			return d, err
		}
		static, err := networkOutput(ctx, "nmcli", "-g", "ipv4.dns", "connection", "show", "uuid", id)
		if err != nil {
			return d, err
		}
		if auto == "no" && strings.TrimSpace(static) != "" {
			return d, &AppError{Code: "DNS_MIXED_CONFIGURATION", Message: "此网络连接同时使用自动和手动 DNS，请先在系统设置中选择一种方式"}
		}
		d.Automatic = auto == "no"
		if !d.Automatic {
			d.Servers = strings.FieldsFunc(static, func(r rune) bool { return r == ',' || r == ' ' || r == '\n' })
		}
	}
	for _, server := range d.Servers {
		if net.ParseIP(server) == nil {
			return d, invalid("无法识别当前 DNS 配置，未修改网络设置")
		}
	}
	if !d.Automatic && len(d.Servers) == 0 {
		return d, invalid("静态 DNS 地址为空")
	}
	return d, nil
}
func dnsApply(ctx context.Context, name string, desired dnsConfiguration) error {
	current, err := dnsRead(ctx, name)
	if err != nil {
		return err
	}
	if current.InterfaceID != desired.InterfaceID {
		return invalid("网络接口已变化")
	}
	if !desired.Automatic && len(desired.Servers) == 0 {
		return invalid("静态 DNS 至少需要一个地址")
	}
	for _, server := range desired.Servers {
		if net.ParseIP(server) == nil {
			return invalid("DNS 服务器地址无效")
		}
	}
	if runtime.GOOS == "darwin" {
		quote := func(s string) string { return "'" + strings.ReplaceAll(s, "'", "'\\''") + "'" }
		command := "/usr/sbin/networksetup -setdnsservers " + quote(name)
		if desired.Automatic {
			command += " empty"
		} else {
			for _, server := range desired.Servers {
				command += " " + quote(server)
			}
		}
		apple := "do shell script \"" + strings.NewReplacer("\\", "\\\\", "\"", "\\\"").Replace(command) + "\" with administrator privileges"
		_, err = networkOutput(ctx, "osascript", "-e", apple)
	} else {
		auto := "yes"
		servers := strings.Join(desired.Servers, ",")
		if desired.Automatic {
			auto = "no"
			servers = ""
		}
		_, err = networkOutput(ctx, "nmcli", "connection", "modify", "uuid", desired.InterfaceID, "ipv4.ignore-auto-dns", auto, "ipv4.dns", servers)
		if err == nil {
			_, err = networkOutput(ctx, "nmcli", "device", "reapply", name)
		}
	}
	if err != nil {
		return err
	}
	after, err := dnsRead(ctx, name)
	if err != nil {
		return err
	}
	if !desired.matches(after) {
		return &AppError{Code: "DNS_UNVERIFIED", Message: "DNS 配置未达到目标状态，请刷新后重试"}
	}
	return nil
}
