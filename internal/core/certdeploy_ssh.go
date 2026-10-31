package core

import (
	"context"
	"errors"
	"net"
	"os"
	"path"
	"path/filepath"
	"strings"
	"time"

	"github.com/pkg/sftp"
	"golang.org/x/crypto/ssh"
	"niceenv/internal/process"
)

func deployCertSSH(ctx context.Context, t certDeployTarget, m certIssuedMaterial) (string, error) {
	port, _ := certSSHPort(t)
	host, address, err := sshEndpoint(t.Config["host"], port)
	if err != nil {
		return "", err
	}
	timeout, _ := certDeployTimeout(t)
	ctx, cancel := context.WithTimeout(ctx, timeout+time.Minute)
	defer cancel()
	var auth ssh.AuthMethod
	if t.Config["auth"] == "key" {
		keyPath := DisplayPath(certSSHIdentity(t))
		if !filepath.IsAbs(keyPath) {
			return "", invalid("SSH 登录私钥须为绝对文件路径")
		}
		body, err := readPrivateMaterial(keyPath)
		if err != nil {
			return "", invalid("无法读取 SSH 登录私钥")
		}
		var signer ssh.Signer
		if pass := t.Config["keyPassphrase"]; pass != "" {
			signer, err = ssh.ParsePrivateKeyWithPassphrase(body, []byte(pass))
		} else {
			signer, err = ssh.ParsePrivateKey(body)
		}
		if err != nil {
			return "", invalid("SSH 登录私钥格式或密码无效")
		}
		auth = ssh.PublicKeys(signer)
	} else {
		auth = ssh.Password(t.Config["password"])
	}
	conn, err := (&net.Dialer{Timeout: 15 * time.Second}).DialContext(ctx, "tcp", address)
	if err != nil {
		return "", invalid("SSH 连接失败")
	}
	defer conn.Close()
	deadline, _ := ctx.Deadline()
	if err = conn.SetDeadline(minTime(deadline, time.Now().Add(20*time.Second))); err != nil {
		return "", err
	}
	stop := context.AfterFunc(ctx, func() { conn.Close() })
	defer stop()
	config := &ssh.ClientConfig{User: t.Config["user"], Auth: []ssh.AuthMethod{auth}, HostKeyCallback: func(_ string, _ net.Addr, key ssh.PublicKey) error {
		if ssh.FingerprintSHA256(key) != t.Config["hostFingerprint"] {
			return &AppError{Code: "SSH_HOST_KEY_CHANGED", Message: "SSH 主机指纹改变，未发送登录凭据"}
		}
		return nil
	}}
	cc, chans, reqs, err := ssh.NewClientConn(conn, address, config)
	if err != nil {
		var app *AppError
		if errors.As(err, &app) {
			return "", app
		}
		return "", invalid("SSH 握手或身份认证失败")
	}
	client := ssh.NewClient(cc, chans, reqs)
	defer client.Close()
	if err = conn.SetDeadline(minTime(deadline, time.Now().Add(time.Minute))); err != nil {
		return "", err
	}
	fs, err := sftp.NewClient(client)
	if err != nil {
		return "", invalid("SSH 服务器未提供 SFTP")
	}
	defer fs.Close()
	if err = certRemotePair(fs, t.Config["certPath"], t.Config["keyPath"], m); err != nil {
		return "", err
	}
	if script := t.Config["script"]; strings.TrimSpace(script) != "" {
		if err = conn.SetDeadline(minTime(deadline, time.Now().Add(timeout))); err != nil {
			return "", err
		}
		session, err := client.NewSession()
		if err != nil {
			return "", &AppError{Code: "DEPLOY_UNCERTAIN", Message: "远程证书已写入，但未能执行部署脚本"}
		}
		defer session.Close()
		session.Stdout = process.NewLimitedBuffer(32768)
		session.Stderr = process.NewLimitedBuffer(32768)
		if err = session.Run(script); err != nil {
			_ = session.Signal(ssh.SIGTERM)
			return "", &AppError{Code: "DEPLOY_UNCERTAIN", Message: "远程证书已写入，部署脚本未成功完成，请检查服务器后手动重试"}
		}
	}
	return "已通过 SFTP 部署证书到 " + host, nil
}
func minTime(a, b time.Time) time.Time {
	if a.Before(b) {
		return a
	}
	return b
}

func certRemotePair(fs *sftp.Client, certPath, keyPath string, m certIssuedMaterial) error {
	type entry struct {
		dest, stage, backup string
		old                 os.FileInfo
		moved, installed    bool
	}
	nonce, err := newID("cert")
	if err != nil {
		return err
	}
	items := []entry{}
	for _, dest := range []string{certPath, keyPath} {
		parent := ""
		parts := strings.Split(strings.TrimPrefix(path.Dir(dest), "/"), "/")
		for _, part := range parts {
			if part == "" {
				continue
			}
			parent += "/" + part
			info, err := fs.Lstat(parent)
			if os.IsNotExist(err) {
				if err = fs.Mkdir(parent); err != nil {
					return err
				}
				if err = fs.Chmod(parent, 0700); err != nil {
					return err
				}
			} else if err != nil {
				return err
			} else if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
				return invalid("SFTP 父路径不是普通目录")
			}
		}
		old, err := fs.Lstat(dest)
		if err != nil && !os.IsNotExist(err) {
			return err
		}
		if err == nil && !old.Mode().IsRegular() {
			return invalid("远程证书目标不是普通文件")
		}
		items = append(items, entry{dest: dest, stage: dest + ".niceenv-" + nonce + ".new", backup: dest + ".niceenv-" + nonce + ".old", old: old})
	}
	result := func() error {
		for i, content := range []string{m.Chain, m.KeyPEM} {
			e := &items[i]
			f, err := fs.OpenFile(e.stage, os.O_WRONLY|os.O_CREATE|os.O_EXCL)
			if err != nil {
				return err
			}
			mode := os.FileMode(0600)
			if i == 0 {
				mode = 0644
			}
			if e.old != nil {
				mode = e.old.Mode().Perm()
				if i == 1 {
					mode &= 0660
				}
				if stat, ok := e.old.Sys().(*sftp.FileStat); ok {
					if err = f.Chown(int(stat.UID), int(stat.GID)); err != nil {
						f.Close()
						return err
					}
				}
			}
			if err = f.Chmod(mode); err == nil {
				_, err = f.Write([]byte(content))
			}
			if err == nil {
				if _, ok := fs.HasExtension("fsync@openssh.com"); ok {
					err = f.Sync()
				}
			}
			err = errors.Join(err, f.Close())
			if err != nil {
				return err
			}
		}
		for i := range items {
			e := &items[i]
			current, err := fs.Lstat(e.dest)
			if err != nil && !os.IsNotExist(err) {
				return err
			}
			if (err == nil) != (e.old != nil) || err == nil && (!current.Mode().IsRegular() || current.Size() != e.old.Size() || !current.ModTime().Equal(e.old.ModTime())) {
				return invalid("远程文件在部署期间发生变化")
			}
			if e.old != nil {
				if err = fs.Rename(e.dest, e.backup); err != nil {
					return err
				}
				e.moved = true
			}
			if err = fs.Rename(e.stage, e.dest); err != nil {
				return err
			}
			e.installed = true
		}
		return nil
	}()
	if result != nil {
		var failures []error
		changed := false
		for i := len(items) - 1; i >= 0; i-- {
			e := items[i]
			changed = changed || e.moved || e.installed
			if e.installed {
				if err = fs.Remove(e.dest); err != nil {
					failures = append(failures, err)
				}
			}
			if e.moved {
				if err = fs.Rename(e.backup, e.dest); err != nil {
					failures = append(failures, err)
				}
			}
			if err = fs.Remove(e.stage); err != nil && !os.IsNotExist(err) {
				failures = append(failures, err)
			}
		}
		if changed || len(failures) > 0 {
			return &AppError{Code: "DEPLOY_UNCERTAIN", Message: "远程证书部署中断，请核对文件和 .niceenv-" + nonce + " 备份后手动重试"}
		}
		return invalid("SFTP 暂存证书失败，原文件未修改")
	}
	// Retain the prior pair deliberately; these are recovery files with the old
	// ownership and permissions, never silently delete an administrator's data.
	return nil
}
