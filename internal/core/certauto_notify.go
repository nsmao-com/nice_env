package core

import (
	"context"
	"crypto/tls"
	"encoding/base64"
	"fmt"
	"mime"
	"net"
	"net/mail"
	"net/smtp"
	"strconv"
	"strings"
	"time"
)

func validateCertSMTP(s *certSMTP) error {
	if s == nil {
		return invalid("请填写 SMTP 配置")
	}
	if _, _, err := sshEndpoint(s.Host, s.Port); err != nil {
		return invalid("SMTP 主机或端口无效")
	}
	for _, address := range []string{s.From, s.To} {
		a, err := mail.ParseAddress(address)
		if err != nil || a.Address != address || strings.ContainsAny(address, "\r\n") {
			return invalid("发件人与收件邮箱须为单个有效邮箱地址")
		}
	}
	if s.Username != "" && s.Password == "" {
		return invalid("请填写 SMTP 登录密码")
	}
	return nil
}
func sendCertEmail(ctx context.Context, s *certSMTP, title, detail string) error {
	if err := validateCertSMTP(s); err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	host, address, _ := sshEndpoint(s.Host, s.Port)
	conn, err := (&net.Dialer{Timeout: 10 * time.Second}).DialContext(ctx, "tcp", address)
	if err != nil {
		return invalid("SMTP 连接失败")
	}
	defer conn.Close()
	deadline, _ := ctx.Deadline()
	if err = conn.SetDeadline(deadline); err != nil {
		return err
	}
	stop := context.AfterFunc(ctx, func() { conn.Close() })
	defer stop()
	tlsConfig := &tls.Config{ServerName: host, MinVersion: tls.VersionTLS12}
	if s.ImplicitTLS {
		secured := tls.Client(conn, tlsConfig)
		if err = secured.HandshakeContext(ctx); err != nil {
			return invalid("SMTP TLS 连接失败")
		}
		conn = secured
	}
	client, err := smtp.NewClient(conn, host)
	if err != nil {
		return invalid("SMTP 服务器握手失败")
	}
	defer client.Close()
	if !s.ImplicitTLS {
		if ok, _ := client.Extension("STARTTLS"); !ok {
			return invalid("SMTP 服务未提供 STARTTLS，未发送登录凭据")
		}
		if err = client.StartTLS(tlsConfig); err != nil {
			return invalid("SMTP STARTTLS 升级失败")
		}
	}
	if s.Username != "" {
		if err = client.Auth(smtp.PlainAuth("", s.Username, s.Password, host)); err != nil {
			return invalid("SMTP 登录被拒绝")
		}
	}
	if err = client.Mail(s.From); err != nil {
		return invalid("SMTP 发件人被拒绝")
	}
	if err = client.Rcpt(s.To); err != nil {
		return invalid("SMTP 收件人被拒绝")
	}
	writer, err := client.Data()
	if err != nil {
		return invalid("SMTP 未接受邮件")
	}
	encoded := base64.StdEncoding.EncodeToString([]byte(detail))
	var lines []string
	for len(encoded) > 76 {
		lines = append(lines, encoded[:76])
		encoded = encoded[76:]
	}
	lines = append(lines, encoded)
	_, err = fmt.Fprintf(writer, "From: %s\r\nTo: %s\r\nSubject: %s\r\nDate: %s\r\nMessage-ID: <%s.%s@niceenv.local>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n%s\r\n", s.From, s.To, mime.QEncoding.Encode("UTF-8", title), time.Now().Format(time.RFC1123Z), strconv.FormatInt(time.Now().UnixNano(), 10), certSHA256(title)[:12], strings.Join(lines, "\r\n"))
	if err != nil {
		return invalid("SMTP 邮件发送未完成")
	}
	if err = writer.Close(); err != nil {
		return invalid("SMTP 未确认接收邮件")
	}
	return client.Quit()
}
