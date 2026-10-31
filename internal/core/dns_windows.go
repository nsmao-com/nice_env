//go:build windows

package core

import (
	"context"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"net"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf16"

	"golang.org/x/sys/windows"
	"niceenv/internal/process"
)

func psQuote(text string) string { return "'" + strings.ReplaceAll(text, "'", "''") + "'" }
func encodedPowerShell(script string) string {
	units := utf16.Encode([]rune(script))
	data := make([]byte, len(units)*2)
	for i, v := range units {
		binary.LittleEndian.PutUint16(data[2*i:], v)
	}
	return base64.StdEncoding.EncodeToString(data)
}
func powershellOutput(ctx context.Context, script string) (string, error) {
	dir, err := windows.GetSystemDirectory()
	if err != nil {
		return "", err
	}
	ctx, cancel := context.WithTimeout(ctx, 45*time.Second)
	defer cancel()
	out := process.NewLimitedBuffer(2 * 1024 * 1024)
	stderr := process.NewLimitedBuffer(16384)
	child, err := process.Start(ctx, process.Spec{Program: filepath.Join(dir, "WindowsPowerShell", "v1.0", "powershell.exe"), Args: []string{"-NoProfile", "-NonInteractive", "-OutputFormat", "Text", "-EncodedCommand", encodedPowerShell("$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); try { " + script + " } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }")}, Stdout: out, Stderr: stderr})
	if err != nil {
		return "", err
	}
	result := child.Wait()
	if result.Code != 0 || result.Err != nil {
		return "", &AppError{Code: "SYSTEM_NETWORK_COMMAND", Message: "读取或修改系统网络配置失败", Detail: stderr.String()}
	}
	return strings.TrimPrefix(out.String(), "\ufeff"), nil
}
func dnsInterfaces(ctx context.Context) ([]string, error) {
	text, err := powershellOutput(ctx, `ConvertTo-Json -Compress -InputObject @(Get-NetAdapter -IncludeHidden | Where-Object { $_.Status -eq 'Up' -and (Test-Path -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\'+([guid]$_.InterfaceGuid).ToString('B'))) } | Select-Object -ExpandProperty Name | Sort-Object -Unique)`)
	if err != nil {
		return nil, err
	}
	result := []string{}
	err = json.Unmarshal([]byte(text), &result)
	return result, err
}
func dnsAdapterScript(name string) string {
	return "$adapter=@(Get-NetAdapter -IncludeHidden | Where-Object { $_.Name -eq " + psQuote(name) + " }); if($adapter.Count -ne 1){throw '网络接口不存在或名称不唯一'}; $id=([guid]$adapter[0].InterfaceGuid).ToString('B'); "
}
func dnsRead(ctx context.Context, name string) (dnsConfiguration, error) {
	var result dnsConfiguration
	if err := dnsName(name); err != nil {
		return result, err
	}
	text, err := powershellOutput(ctx, dnsAdapterScript(name)+`$registry=Get-ItemProperty -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\'+$id); $static=[string]$registry.NameServer; $servers=@((Get-DnsClientServerAddress -InterfaceIndex $adapter[0].ifIndex -AddressFamily IPv4).ServerAddresses); @{interfaceId=$id;automatic=[string]::IsNullOrWhiteSpace($static);servers=$servers}|ConvertTo-Json -Compress`)
	if err != nil {
		return result, err
	}
	err = json.Unmarshal([]byte(text), &result)
	return result, err
}
func dnsApply(ctx context.Context, name string, desired dnsConfiguration) error {
	current, err := dnsRead(ctx, name)
	if err != nil {
		return err
	}
	if current.InterfaceID != desired.InterfaceID {
		return invalid("网络接口已变化，不能写入旧配置")
	}
	if !desired.Automatic && len(desired.Servers) == 0 {
		return invalid("静态 DNS 至少需要一个地址")
	}
	for _, server := range desired.Servers {
		ip := net.ParseIP(server)
		if ip == nil || ip.To4() == nil {
			return invalid("DNS 地址必须为 IPv4 地址")
		}
	}
	dir, err := windows.GetSystemDirectory()
	if err != nil {
		return err
	}
	netsh := psQuote(filepath.Join(dir, "netsh.exe"))
	arg := psQuote("name=" + name)
	script := "$ErrorActionPreference='Stop'; " + dnsAdapterScript(name) + "if($id -ne " + psQuote(desired.InterfaceID) + "){throw '网络接口已变化'}; "
	if desired.Automatic {
		script += "& " + netsh + " interface ipv4 set dnsservers " + arg + " source=dhcp; exit $LASTEXITCODE"
	} else {
		script += "& " + netsh + " interface ipv4 set dnsservers " + arg + " source=static address=" + desired.Servers[0] + " validate=no; if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}; "
		for i, server := range desired.Servers[1:] {
			script += fmt.Sprintf("& %s interface ipv4 add dnsservers %s address=%s index=%d validate=no; if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}; ", netsh, arg, server, i+2)
		}
		script += "exit 0"
	}
	// The elevated payload is constructed only from validated DNS addresses and quoted names.
	// No caller-supplied script or executable is accepted by this command.
	ps := filepath.Join(dir, "WindowsPowerShell", "v1.0", "powershell.exe")
	_, err = powershellOutput(ctx, "$p=Start-Process -FilePath "+psQuote(ps)+" -ArgumentList @('-NoProfile','-NonInteractive','-EncodedCommand',"+psQuote(encodedPowerShell(script))+") -Verb RunAs -WindowStyle Hidden -PassThru -Wait; if($p.ExitCode -ne 0){throw ('DNS 管理员操作未完成：'+$p.ExitCode)}")
	if err != nil {
		return err
	}
	after, err := dnsRead(ctx, name)
	if err != nil {
		return err
	}
	if !desired.matches(after) {
		return &AppError{Code: "DNS_UNVERIFIED", Message: "DNS 配置未达到目标状态，请刷新核对后重试"}
	}
	return nil
}
