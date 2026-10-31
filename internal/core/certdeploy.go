package core

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"strconv"
	"strings"
	"time"
	"unicode"

	"golang.org/x/crypto/ssh"
)

type sshHostKey struct {
	Host        string `json:"host"`
	Port        int    `json:"port"`
	Fingerprint string `json:"fingerprint"`
}

func sshEndpoint(host string, port int) (string, string, error) {
	host = strings.Trim(strings.TrimSpace(host), "[]")
	if host == "" || len(host) > 253 || port < 1 || port > 65535 || strings.IndexFunc(host, func(r rune) bool { return unicode.IsSpace(r) || unicode.IsControl(r) }) >= 0 {
		return "", "", invalid("请填写有效的 SSH 主机和 1–65535 的端口")
	}
	if net.ParseIP(host) == nil {
		for _, label := range strings.Split(strings.TrimSuffix(host, "."), ".") {
			if len(label) == 0 || len(label) > 63 || strings.HasPrefix(label, "-") || strings.HasSuffix(label, "-") {
				return "", "", invalid("SSH 主机名无效")
			}
			for _, ch := range label {
				if !(ch >= 'a' && ch <= 'z' || ch >= 'A' && ch <= 'Z' || ch >= '0' && ch <= '9' || ch == '-') {
					return "", "", invalid("SSH 主机不能包含协议、路径或认证信息")
				}
			}
		}
	}
	return host, net.JoinHostPort(host, strconv.Itoa(port)), nil
}

func (c *Core) registerCertificateDeployment() {
	c.handle("certdeploy_probe_ssh", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Host string `json:"host"`
			Port int    `json:"port"`
		}](data)
		if err != nil {
			return nil, err
		}
		host, address, err := sshEndpoint(in.Host, in.Port)
		if err != nil {
			return nil, err
		}
		ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
		defer cancel()
		conn, err := (&net.Dialer{}).DialContext(ctx, "tcp", address)
		if err != nil {
			return nil, &AppError{Code: "SSH_CONNECT", Message: "无法连接 SSH 主机，请检查地址、端口和网络"}
		}
		defer conn.Close()
		deadline, _ := ctx.Deadline()
		if err = conn.SetDeadline(deadline); err != nil {
			return nil, err
		}
		stop := context.AfterFunc(ctx, func() { conn.Close() })
		defer stop()
		fingerprint := ""
		stopHandshake := errors.New("host key captured; authentication disabled")
		config := &ssh.ClientConfig{User: "niceenv-host-key-probe", HostKeyCallback: func(_ string, _ net.Addr, key ssh.PublicKey) error {
			fingerprint = ssh.FingerprintSHA256(key)
			return stopHandshake
		}}
		// The callback deliberately rejects the key after reading it. No username,
		// password or private-key authentication request is sent to the remote host.
		client, _, _, handshakeErr := ssh.NewClientConn(conn, address, config)
		if client != nil {
			client.Close()
		}
		if fingerprint != "" && errors.Is(handshakeErr, stopHandshake) {
			return sshHostKey{Host: host, Port: in.Port, Fingerprint: fingerprint}, nil
		}
		if ctx.Err() != nil {
			return nil, &AppError{Code: "SSH_CONNECT", Message: "读取 SSH 主机指纹超时或已取消"}
		}
		return nil, &AppError{Code: "SSH_CONNECT", Message: "服务端未提供可验证的 SSH 主机密钥"}
	})
}
