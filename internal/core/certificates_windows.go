//go:build windows

package core

import (
	"bytes"
	"context"
	"crypto/sha1"
	"crypto/x509"
	"runtime"
	"unsafe"

	"golang.org/x/sys/windows"
)

func certificateTrusted(cert *x509.Certificate) bool {
	name, _ := windows.UTF16PtrFromString("ROOT")
	hash := sha1.Sum(cert.Raw)
	blob := windows.CryptDataBlob{Size: uint32(len(hash)), Data: &hash[0]}
	for _, location := range []uint32{windows.CERT_SYSTEM_STORE_CURRENT_USER, windows.CERT_SYSTEM_STORE_LOCAL_MACHINE} {
		store, err := windows.CertOpenStore(windows.CERT_STORE_PROV_SYSTEM_W, 0, 0, location|windows.CERT_STORE_READONLY_FLAG|windows.CERT_STORE_OPEN_EXISTING_FLAG, uintptr(unsafe.Pointer(name)))
		runtime.KeepAlive(name)
		if err != nil {
			continue
		}
		found, err := windows.CertFindCertificateInStore(store, windows.X509_ASN_ENCODING|windows.PKCS_7_ASN_ENCODING, 0, windows.CERT_FIND_SHA1_HASH, unsafe.Pointer(&blob), nil)
		valid := false
		if err == nil && found != nil {
			valid = bytes.Equal(unsafe.Slice(found.EncodedCert, found.Length), cert.Raw)
			windows.CertFreeCertificateContext(found)
		}
		windows.CertCloseStore(store, 0)
		if valid {
			return true
		}
	}
	return false
}
func trustCertificate(ctx context.Context, cert *x509.Certificate) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if !cert.IsCA || cert.CheckSignatureFrom(cert) != nil {
		return invalid("仅能信任匹配的本地自签名根 CA")
	}
	name, _ := windows.UTF16PtrFromString("ROOT")
	store, err := windows.CertOpenStore(windows.CERT_STORE_PROV_SYSTEM_W, 0, 0, windows.CERT_SYSTEM_STORE_CURRENT_USER, uintptr(unsafe.Pointer(name)))
	runtime.KeepAlive(name)
	if err != nil {
		return err
	}
	defer windows.CertCloseStore(store, 0)
	certificate, err := windows.CertCreateCertificateContext(windows.X509_ASN_ENCODING, &cert.Raw[0], uint32(len(cert.Raw)))
	if err != nil {
		return err
	}
	defer windows.CertFreeCertificateContext(certificate)
	// The desktop user's trust store is sufficient for their browsers and avoids
	// elevating the application or changing trust for other Windows accounts.
	return windows.CertAddCertificateContextToStore(store, certificate, windows.CERT_STORE_ADD_REPLACE_EXISTING, nil)
}
