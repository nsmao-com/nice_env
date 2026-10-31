//go:build !windows

package core

import (
	"context"
	"crypto/x509"
	"encoding/pem"
	"os"
	"path/filepath"
	"runtime"
	"time"

	"niceenv/internal/process"
)

func certificateTrusted(cert *x509.Certificate) bool {
	_, err := cert.Verify(x509.VerifyOptions{KeyUsages: []x509.ExtKeyUsage{x509.ExtKeyUsageAny}})
	return err == nil
}
func trustCertificate(ctx context.Context, cert *x509.Certificate) error {
	if runtime.GOOS != "darwin" {
		return invalid("此平台尚未提供系统证书信任管理")
	}
	if !cert.IsCA || cert.CheckSignatureFrom(cert) != nil {
		return invalid("仅能信任匹配的本地自签名根 CA")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	f, err := os.CreateTemp("", "niceenv-root-*.pem")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	if err = pem.Encode(f, &pem.Block{Type: "CERTIFICATE", Bytes: cert.Raw}); err != nil {
		f.Close()
		return err
	}
	if err = f.Close(); err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	output := process.NewLimitedBuffer(4096)
	child, err := process.Start(ctx, process.Spec{Program: "/usr/bin/security", Args: []string{"add-trusted-cert", "-r", "trustRoot", "-k", filepath.Join(home, "Library", "Keychains", "login.keychain-db"), f.Name()}, Stdout: output, Stderr: output})
	if err != nil {
		return err
	}
	result := child.Wait()
	if result.Err != nil || result.Code != 0 {
		return &AppError{Code: "CA_TRUST_FAILED", Message: "根 CA 信任未完成，请确认钥匙串授权", Detail: output.String()}
	}
	return nil
}
