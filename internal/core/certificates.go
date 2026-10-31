package core

import (
	"bytes"
	"context"
	"crypto"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"math"
	"math/big"
	"net"
	"net/netip"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"niceenv/internal/process"
)

type certRecord struct {
	ID        string   `json:"id"`
	Kind      string   `json:"kind"`
	Subject   string   `json:"subject"`
	SANs      []string `json:"sans"`
	NotBefore int64    `json:"notBefore"`
	NotAfter  int64    `json:"notAfter"`
	CertPath  Path     `json:"certPath"`
	KeyPath   Path     `json:"keyPath"`
	Trusted   *bool    `json:"trusted,omitempty"`
}
type certChoice struct {
	ID          string   `json:"id"`
	Kind        string   `json:"kind"`
	Subject     string   `json:"subject"`
	SANs        []string `json:"sans"`
	NotBefore   int64    `json:"notBefore"`
	NotAfter    int64    `json:"notAfter"`
	DaysLeft    int64    `json:"daysLeft"`
	Usable      bool     `json:"usable"`
	Problem     string   `json:"problem,omitempty"`
	UsedBySites []string `json:"usedBySites"`
	CertPath    Path     `json:"certPath"`
	KeyPath     Path     `json:"keyPath"`
}
type certSite struct {
	ID, Name string
	Domains  []string
	HTTPS    bool
	Runtime  struct {
		ImportedCertID string `json:"importedCertId"`
		ACMECertID     string `json:"acmeCertId"`
		WebServer      string `json:"webServer"`
	}
}

func normalizeCertDomains(input []string) ([]string, error) {
	if len(input) < 1 || len(input) > 100 {
		return nil, invalid("请填写 1 至 100 个域名或 IP 地址")
	}
	out := []string{}
	seen := map[string]bool{}
	for _, raw := range input {
		d := strings.ToLower(strings.TrimRight(strings.TrimSpace(raw), "."))
		if ip, err := netip.ParseAddr(d); err == nil && ip.Zone() == "" {
			d = ip.String()
		} else {
			host := strings.TrimPrefix(d, "*.")
			if len(host) > 253 || host != "localhost" && !strings.Contains(host, ".") {
				return nil, invalid("证书域名格式无效")
			}
			if d != host && host == "localhost" {
				return nil, invalid("localhost 不支持通配符")
			}
			if strings.Trim(host, "0123456789.") == "" {
				return nil, invalid("IP 地址格式无效")
			}
			for _, label := range strings.Split(host, ".") {
				if label == "" || len(label) > 63 || strings.HasPrefix(label, "-") || strings.HasSuffix(label, "-") {
					return nil, invalid("证书域名格式无效")
				}
				for _, ch := range label {
					if !(ch >= 'a' && ch <= 'z' || ch >= '0' && ch <= '9' || ch == '-') {
						return nil, invalid("证书域名不能包含协议、端口或路径")
					}
				}
			}
		}
		if !seen[d] {
			out = append(out, d)
			seen[d] = true
		}
	}
	return out, nil
}
func certStem(domain string) string {
	return strings.NewReplacer("*", "_wildcard", ":", "_").Replace(domain)
}
func certSANs(cert *x509.Certificate) []string {
	out := append([]string{}, cert.DNSNames...)
	for _, ip := range cert.IPAddresses {
		out = append(out, ip.String())
	}
	return out
}
func certCovers(cert *x509.Certificate, domain string) bool {
	if strings.HasPrefix(domain, "*.") {
		for _, san := range cert.DNSNames {
			if strings.EqualFold(san, domain) {
				return true
			}
		}
		return false
	}
	return cert.VerifyHostname(domain) == nil
}
func certValidity(cert *x509.Certificate, server bool) error {
	now := time.Now()
	if now.Before(cert.NotBefore) {
		return invalid("证书尚未生效")
	}
	if !now.Before(cert.NotAfter) {
		return invalid("证书已过期")
	}
	if server {
		if cert.IsCA {
			return invalid("根证书或中间 CA 不能作为 HTTPS 站点证书")
		}
		allowed := len(cert.ExtKeyUsage) == 0 && len(cert.UnknownExtKeyUsage) == 0
		for _, usage := range cert.ExtKeyUsage {
			allowed = allowed || usage == x509.ExtKeyUsageServerAuth || usage == x509.ExtKeyUsageAny
		}
		if !allowed {
			return invalid("证书不允许用于 HTTPS 服务器")
		}
		if len(certSANs(cert)) == 0 {
			return invalid("证书没有 DNS 或 IP SAN")
		}
	}
	return nil
}
func parseCertChain(data []byte) ([]*x509.Certificate, error) {
	chain := []*x509.Certificate{}
	for len(bytes.TrimSpace(data)) > 0 {
		block, rest := pem.Decode(data)
		if block == nil || block.Type != "CERTIFICATE" {
			return nil, invalid("证书必须为 PEM 证书链")
		}
		cert, err := x509.ParseCertificate(block.Bytes)
		if err != nil {
			return nil, invalid("证书内容无法解析")
		}
		chain = append(chain, cert)
		data = rest
		if len(chain) > 32 {
			return nil, invalid("证书链过长")
		}
	}
	if len(chain) == 0 {
		return nil, invalid("文件中没有证书")
	}
	for i := 1; i < len(chain); i++ {
		if err := chain[i-1].CheckSignatureFrom(chain[i]); err != nil {
			return nil, invalid("证书链顺序错误或签名不匹配")
		}
	}
	return chain, nil
}
func certPair(certPEM, keyPEM []byte) ([]*x509.Certificate, crypto.Signer, error) {
	chain, err := parseCertChain(certPEM)
	if err != nil {
		return nil, nil, err
	}
	pair, err := tls.X509KeyPair(certPEM, keyPEM)
	if err != nil {
		return nil, nil, invalid("证书和私钥不匹配或私钥格式无效")
	}
	signer, ok := pair.PrivateKey.(crypto.Signer)
	if !ok {
		return nil, nil, invalid("不支持此私钥类型")
	}
	return chain, signer, nil
}
func readCertificateSource(path string) ([]byte, error) {
	if !filepath.IsAbs(path) {
		return nil, invalid("请选择证书的绝对路径")
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, invalid("请选择普通证书文件")
	}
	data, err := io.ReadAll(io.LimitReader(f, 4*1024*1024+1))
	if err != nil {
		return nil, err
	}
	if len(data) > 4*1024*1024 {
		return nil, invalid("证书文件超过 4 MiB")
	}
	return data, nil
}
func (c *Core) readCertFile(path string) ([]byte, error) {
	path = DisplayPath(path)
	rel, err := filepath.Rel(c.Home, path)
	if err != nil || !filepath.IsLocal(rel) || !strings.HasPrefix(filepath.ToSlash(rel), "certs/") {
		return nil, invalid("证书路径不在托管目录中")
	}
	root, err := os.OpenRoot(c.Home)
	if err != nil {
		return nil, err
	}
	defer root.Close()
	info, err := siteCheck(root, rel, false)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, invalid("托管证书必须为普通文件")
	}
	f, err := root.Open(rel)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, 4*1024*1024+1))
	if err != nil {
		return nil, err
	}
	if len(data) > 4*1024*1024 {
		return nil, invalid("证书文件超过 4 MiB")
	}
	return data, nil
}
func (c *Core) certificateRecords(ctx context.Context) ([]certRecord, error) {
	rows, err := c.Store.db.QueryContext(ctx, "SELECT id,kind,subject,sans,not_before,not_after,cert_path,COALESCE(key_path,'') FROM certs ORDER BY not_after DESC")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []certRecord{}
	for rows.Next() {
		var r certRecord
		var raw string
		if err = rows.Scan(&r.ID, &r.Kind, &r.Subject, &raw, &r.NotBefore, &r.NotAfter, &r.CertPath, &r.KeyPath); err != nil {
			return nil, err
		}
		if err = json.Unmarshal([]byte(raw), &r.SANs); err != nil {
			return nil, invalid("证书记录 SAN 格式无效")
		}
		out = append(out, r)
	}
	return out, rows.Err()
}
func (c *Core) certificateSites(ctx context.Context) ([]certSite, error) {
	rows, err := c.Store.db.QueryContext(ctx, "SELECT id,name,domains,https,runtime FROM sites")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []certSite{}
	for rows.Next() {
		var s certSite
		var domains, runtime string
		if err = rows.Scan(&s.ID, &s.Name, &domains, &s.HTTPS, &runtime); err != nil {
			return nil, err
		}
		if json.Unmarshal([]byte(domains), &s.Domains) != nil || json.Unmarshal([]byte(runtime), &s.Runtime) != nil {
			return nil, invalid("站点证书绑定记录无效")
		}
		out = append(out, s)
	}
	return out, rows.Err()
}
func certUsed(site certSite, record certRecord) bool {
	if record.Kind == "imported" {
		return site.Runtime.ImportedCertID == record.ID
	}
	if site.Runtime.ImportedCertID != "" || record.Kind != "site" && record.Kind != "acme" {
		return false
	}
	if site.Runtime.ACMECertID != "" {
		return record.Kind == "acme" && site.Runtime.ACMECertID == record.ID
	}
	return len(site.Domains) > 0 && strings.EqualFold(site.Domains[0], record.Subject)
}
func (c *Core) saveCertRecord(ctx context.Context, r certRecord) error {
	sans, err := json.Marshal(r.SANs)
	if err != nil {
		return err
	}
	tx, err := c.Store.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err = tx.ExecContext(ctx, "DELETE FROM certs WHERE cert_path=? AND id<>?", string(r.CertPath), r.ID); err != nil {
		return err
	}
	_, err = tx.ExecContext(ctx, `INSERT INTO certs(id,kind,subject,sans,not_before,not_after,cert_path,key_path) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,subject=excluded.subject,sans=excluded.sans,not_before=excluded.not_before,not_after=excluded.not_after,cert_path=excluded.cert_path,key_path=excluded.key_path`, r.ID, r.Kind, r.Subject, string(sans), r.NotBefore, r.NotAfter, string(r.CertPath), string(r.KeyPath))
	if err != nil {
		return err
	}
	return tx.Commit()
}

// The persistent backup contains both originals before either member changes.
// Caller holds site, service and certificate locks through reload/commit.
func (c *Core) writeCertPair(ctx context.Context, certPath, keyPath string, certPEM, keyPEM []byte, commit func() error) error {
	if _, _, err := certPair(certPEM, keyPEM); err != nil {
		return err
	}
	paths := []string{certPath, keyPath}
	before := make([][]byte, 2)
	exists := make([]bool, 2)
	for i, path := range paths {
		if err := managedDirectory(c.Home, filepath.Dir(path)); err != nil {
			return err
		}
		data, err := c.readCertFile(path)
		if err != nil && !os.IsNotExist(err) {
			return err
		}
		before[i] = data
		exists[i] = err == nil
	}
	backupRoot := filepath.Join(c.Home, "backup", "certificates")
	if err := managedDirectory(c.Home, backupRoot); err != nil {
		return err
	}
	backup, err := os.MkdirTemp(backupRoot, "pair-")
	if err != nil {
		return err
	}
	metadata, _ := json.Marshal(map[string]any{"paths": paths, "existed": exists})
	if err = atomicWrite(filepath.Join(backup, "restore.json"), metadata); err != nil {
		return err
	}
	for i, data := range before {
		if exists[i] {
			if err = atomicWrite(filepath.Join(backup, fmt.Sprintf("%d.bak", i)), data); err != nil {
				return err
			}
		}
	}
	written := 0
	result := func() error {
		for i, data := range [][]byte{certPEM, keyPEM} {
			if err := ctx.Err(); err != nil {
				return err
			}
			current, err := c.readCertFile(paths[i])
			if err != nil && !os.IsNotExist(err) {
				return err
			}
			if (err == nil) != exists[i] || !bytes.Equal(current, before[i]) {
				return invalid("证书文件在保存前发生变化")
			}
			if err = atomicWrite(paths[i], data); err != nil {
				return err
			}
			written++
		}
		return commit()
	}()
	if result == nil {
		return nil
	}
	failures := []error{result}
	for i := written - 1; i >= 0; i-- {
		var err error
		if exists[i] {
			err = atomicWrite(paths[i], before[i])
		} else {
			err = os.Remove(paths[i])
		}
		if err != nil {
			failures = append(failures, err)
		}
	}
	if len(failures) > 1 {
		return &AppError{Code: "CERT_ROLLBACK_FAILED", Message: "证书保存失败，部分文件未能恢复", Hint: "恢复副本：" + DisplayPath(backup), Detail: errors.Join(failures...).Error()}
	}
	return result
}
func (c *Core) loadCA() (*x509.Certificate, crypto.Signer, error) {
	certPEM, err := c.readCertFile(filepath.Join(c.Home, "certs", "ca.crt"))
	if err != nil {
		return nil, nil, err
	}
	keyPEM, err := c.readCertFile(filepath.Join(c.Home, "certs", "ca.key"))
	if err != nil {
		return nil, nil, err
	}
	chain, key, err := certPair(certPEM, keyPEM)
	if err != nil {
		return nil, nil, err
	}
	ca := chain[0]
	if !ca.IsCA || ca.CheckSignatureFrom(ca) != nil {
		return nil, nil, invalid("本地根证书不是有效的自签名 CA")
	}
	if err = certValidity(ca, false); err != nil {
		return nil, nil, err
	}
	return ca, key, nil
}
func makeCert(template, parent *x509.Certificate, key, issuer crypto.Signer) ([]byte, []byte, error) {
	serial, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 159))
	if err != nil {
		return nil, nil, err
	}
	template.SerialNumber = serial
	der, err := x509.CreateCertificate(rand.Reader, template, parent, key.Public(), issuer)
	if err != nil {
		return nil, nil, err
	}
	keyDER, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		return nil, nil, err
	}
	return pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER}), nil
}
func (c *Core) ensureCA(ctx context.Context) (*x509.Certificate, crypto.Signer, error) {
	certPath := filepath.Join(c.Home, "certs", "ca.crt")
	keyPath := filepath.Join(c.Home, "certs", "ca.key")
	_, certErr := os.Lstat(certPath)
	_, keyErr := os.Lstat(keyPath)
	if certErr == nil && keyErr == nil {
		return c.loadCA()
	}
	if !os.IsNotExist(certErr) || !os.IsNotExist(keyErr) {
		return nil, nil, &AppError{Code: "CA_INCOMPLETE", Message: "根 CA 文件不完整或无法读取，请恢复原来的 ca.crt 与 ca.key"}
	}
	var count int
	if err := c.Store.db.QueryRowContext(ctx, "SELECT COUNT(*) FROM certs WHERE kind IN ('ca','site')").Scan(&count); err != nil {
		return nil, nil, err
	}
	if count > 0 {
		return nil, nil, &AppError{Code: "CA_MISSING", Message: "根 CA 丢失但仍有本地证书记录，请恢复原 CA"}
	}
	entries, err := os.ReadDir(filepath.Join(c.Home, "certs", "sites"))
	if err != nil && !os.IsNotExist(err) {
		return nil, nil, err
	}
	if len(entries) > 0 {
		return nil, nil, &AppError{Code: "CA_MISSING", Message: "已有站点证书文件，未自动更换根 CA，请恢复原 CA"}
	}
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, nil, err
	}
	now := time.Now()
	ca := &x509.Certificate{Subject: pkix.Name{CommonName: "NiceEnv Local Root CA", Organization: []string{"NiceEnv"}}, NotBefore: now.Add(-24 * time.Hour), NotAfter: now.AddDate(10, 0, 0), IsCA: true, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign | x509.KeyUsageCRLSign}
	certPEM, keyPEM, err := makeCert(ca, ca, key, key)
	if err != nil {
		return nil, nil, err
	}
	if err = c.writeCertPair(ctx, certPath, keyPath, certPEM, keyPEM, func() error { return nil }); err != nil {
		return nil, nil, err
	}
	return c.loadCA()
}
func (c *Core) issueCertificate(ctx context.Context, domains []string) (certRecord, error) {
	domains, err := normalizeCertDomains(domains)
	if err != nil {
		return certRecord{}, err
	}
	sites, err := c.certificateSites(ctx)
	if err != nil {
		return certRecord{}, err
	}
	for _, s := range sites {
		if s.Runtime.ACMECertID == "acme-"+domains[0] {
			return certRecord{}, &AppError{Code: "CERT_IN_USE", Message: "此输出文件仍绑定为 ACME 证书，请先调整站点证书选择"}
		}
	}
	ca, issuer, err := c.ensureCA(ctx)
	if err != nil {
		return certRecord{}, err
	}
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return certRecord{}, err
	}
	now := time.Now()
	expiry := now.Add(30 * 24 * time.Hour)
	if expiry.After(ca.NotAfter) {
		expiry = ca.NotAfter
	}
	leaf := &x509.Certificate{Subject: pkix.Name{CommonName: domains[0]}, NotBefore: now.Add(-24 * time.Hour), NotAfter: expiry, KeyUsage: x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth, x509.ExtKeyUsageClientAuth}, BasicConstraintsValid: true}
	for _, domain := range domains {
		if ip := net.ParseIP(domain); ip != nil {
			leaf.IPAddresses = append(leaf.IPAddresses, ip)
		} else {
			leaf.DNSNames = append(leaf.DNSNames, domain)
		}
	}
	r := certRecord{ID: "cert-" + domains[0], Kind: "site", Subject: domains[0], SANs: domains, NotBefore: leaf.NotBefore.UnixMilli(), NotAfter: leaf.NotAfter.UnixMilli(), CertPath: Path(filepath.Join(c.Home, "certs", "sites", certStem(domains[0])+".crt")), KeyPath: Path(filepath.Join(c.Home, "certs", "sites", certStem(domains[0])+".key"))}
	for _, s := range sites {
		if s.HTTPS && certUsed(s, r) {
			for _, domain := range s.Domains {
				if !certCovers(leaf, domain) {
					return certRecord{}, invalid("新证书没有覆盖使用此文件的站点域名：" + domain)
				}
			}
		}
	}
	certPEM, keyPEM, err := makeCert(leaf, ca, key, issuer)
	if err != nil {
		return certRecord{}, err
	}
	err = c.writeCertPair(ctx, string(r.CertPath), string(r.KeyPath), certPEM, keyPEM, func() error {
		if err := c.reloadNginxPools(ctx); err != nil {
			return err
		}
		return c.saveCertRecord(ctx, r)
	})
	if err != nil {
		_ = c.reloadNginxPools(context.Background())
	}
	return r, err
}

func (c *Core) certLock() (func(), error) {
	if !c.siteChanges.TryLock() {
		return nil, serviceBusy()
	}
	if !c.services.lifecycle.TryLock() {
		c.siteChanges.Unlock()
		return nil, serviceBusy()
	}
	if !c.certChanges.TryLock() {
		c.services.lifecycle.Unlock()
		c.siteChanges.Unlock()
		return nil, serviceBusy()
	}
	unlockLocal := func() { c.certChanges.Unlock(); c.services.lifecycle.Unlock(); c.siteChanges.Unlock() }
	dir := filepath.Join(c.Home, "service-locks")
	if err := managedDirectory(c.Home, dir); err != nil {
		unlockLocal()
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(dir, "certificates.lock"))
	if err != nil {
		unlockLocal()
		return nil, err
	}
	return func() { _ = unlock(); unlockLocal() }, nil
}
func (c *Core) listCertificates(ctx context.Context) ([]certRecord, error) {
	records, err := c.certificateRecords(ctx)
	if err != nil {
		return nil, err
	}
	out := []certRecord{}
	if ca, _, err := c.loadCA(); err == nil {
		trusted := certificateTrusted(ca)
		out = append(out, certRecord{ID: "ca", Kind: "ca", Subject: ca.Subject.CommonName, SANs: []string{}, NotBefore: ca.NotBefore.UnixMilli(), NotAfter: ca.NotAfter.UnixMilli(), CertPath: Path(filepath.Join(c.Home, "certs", "ca.crt")), KeyPath: Path(filepath.Join(c.Home, "certs", "ca.key")), Trusted: &trusted})
	}
	for _, r := range records {
		if r.Kind != "ca" {
			out = append(out, r)
		}
	}
	return out, nil
}
func (c *Core) certificateRecord(ctx context.Context, id string) (certRecord, error) {
	if strings.HasPrefix(id, "imported:") {
		id = strings.TrimPrefix(id, "imported:")
		if !safePart(id) {
			return certRecord{}, invalid("导入证书标识无效")
		}
		return certRecord{ID: id, Kind: "imported", CertPath: Path(filepath.Join(c.Home, "certs", "imported", id+".crt")), KeyPath: Path(filepath.Join(c.Home, "certs", "imported", id+".key"))}, nil
	}
	records, err := c.listCertificates(ctx)
	if err != nil {
		return certRecord{}, err
	}
	for _, r := range records {
		if r.ID == id {
			if err := c.validateCertificatePath(r); err != nil {
				return certRecord{}, err
			}
			return r, nil
		}
	}
	return certRecord{}, &AppError{Code: "NOT_FOUND", Message: "证书不存在，请刷新列表"}
}

func (c *Core) validateCertificatePath(r certRecord) error {
	var stem, dir string
	switch r.Kind {
	case "ca":
		if r.ID != "ca" {
			return invalid("根证书记录无效")
		}
		stem = "ca"
		dir = filepath.Join(c.Home, "certs")
	case "imported":
		if !safePart(r.ID) {
			return invalid("导入证书标识无效")
		}
		stem = r.ID
		dir = filepath.Join(c.Home, "certs", "imported")
	case "site", "acme":
		domains, err := normalizeCertDomains([]string{r.Subject})
		if err != nil || domains[0] != r.Subject {
			return invalid("证书主域名记录无效")
		}
		prefix := "cert-"
		if r.Kind == "acme" {
			prefix = "acme-"
		}
		if r.ID != prefix+r.Subject {
			return invalid("证书标识和主域名不一致")
		}
		stem = certStem(r.Subject)
		dir = filepath.Join(c.Home, "certs", "sites")
	default:
		return invalid("证书类型无效")
	}
	for _, pair := range [][2]string{{string(r.CertPath), filepath.Join(dir, stem+".crt")}, {string(r.KeyPath), filepath.Join(dir, stem+".key")}} {
		rel, err := filepath.Rel(pair[1], DisplayPath(pair[0]))
		if err != nil || rel != "." {
			return invalid("证书记录与托管文件位置不一致")
		}
	}
	return nil
}
func (c *Core) importCertificate(ctx context.Context, certPath, keyPath, id string) (certChoice, error) {
	certPEM, err := readCertificateSource(certPath)
	if err != nil {
		return certChoice{}, err
	}
	keyPEM, err := readCertificateSource(keyPath)
	if err != nil {
		return certChoice{}, err
	}
	chain, _, err := certPair(certPEM, keyPEM)
	if err != nil {
		return certChoice{}, err
	}
	leaf := chain[0]
	if err = certValidity(leaf, true); err != nil {
		return certChoice{}, err
	}
	if id == "" {
		id, err = newID("cert")
		if err != nil {
			return certChoice{}, err
		}
	} else {
		if !safePart(id) {
			return certChoice{}, invalid("导入证书标识无效")
		}
	}
	r := certRecord{ID: id, Kind: "imported", CertPath: Path(filepath.Join(c.Home, "certs", "imported", id+".crt")), KeyPath: Path(filepath.Join(c.Home, "certs", "imported", id+".key"))}
	sites, err := c.certificateSites(ctx)
	if err != nil {
		return certChoice{}, err
	}
	for _, s := range sites {
		if certUsed(s, r) {
			for _, domain := range s.Domains {
				if !certCovers(leaf, domain) {
					return certChoice{}, invalid("新证书未覆盖已绑定的站点域名：" + domain)
				}
			}
		}
	}
	err = c.writeCertPair(ctx, string(r.CertPath), string(r.KeyPath), certPEM, keyPEM, func() error { return c.reloadNginxPools(ctx) })
	if err != nil {
		_ = c.reloadNginxPools(context.Background())
		return certChoice{}, err
	}
	return c.inspectCertificate(r, sites), nil
}
func (c *Core) inspectCertificate(r certRecord, sites []certSite) certChoice {
	out := certChoice{ID: r.ID, Kind: r.Kind, Subject: r.Subject, SANs: []string{}, UsedBySites: []string{}, CertPath: r.CertPath, KeyPath: r.KeyPath}
	for _, s := range sites {
		if certUsed(s, r) {
			out.UsedBySites = append(out.UsedBySites, s.Name)
		}
	}
	err := func() error {
		if err := c.validateCertificatePath(r); err != nil {
			return err
		}
		certPEM, err := c.readCertFile(string(r.CertPath))
		if err != nil {
			return invalid("证书文件缺失或不可读")
		}
		chain, err := parseCertChain(certPEM)
		if err != nil {
			return err
		}
		leaf := chain[0]
		out.Subject = leaf.Subject.CommonName
		out.SANs = certSANs(leaf)
		out.NotBefore = leaf.NotBefore.Unix()
		out.NotAfter = leaf.NotAfter.Unix()
		out.DaysLeft = int64(math.Floor(time.Until(leaf.NotAfter).Hours() / 24))
		keyPEM, err := c.readCertFile(string(r.KeyPath))
		if err != nil {
			return invalid("私钥文件缺失或不可读")
		}
		if _, _, err = certPair(certPEM, keyPEM); err != nil {
			return err
		}
		if err = certValidity(leaf, r.Kind != "ca"); err != nil {
			return err
		}
		if r.Kind == "ca" && !leaf.IsCA {
			return invalid("证书不是根 CA")
		}
		for _, s := range sites {
			if s.HTTPS && certUsed(s, r) {
				for _, domain := range s.Domains {
					if !certCovers(leaf, domain) {
						return invalid("证书未覆盖域名：" + domain)
					}
				}
			}
		}
		return nil
	}()
	out.Usable = err == nil
	if err != nil {
		out.Problem = publicError(err).Message
	}
	return out
}
func (c *Core) importedCertificates(ctx context.Context) ([]certChoice, error) {
	dir := filepath.Join(c.Home, "certs", "imported")
	entries, err := os.ReadDir(dir)
	if err != nil && !os.IsNotExist(err) {
		return nil, err
	}
	ids := map[string]bool{}
	for _, entry := range entries {
		ext := filepath.Ext(entry.Name())
		if ext == ".crt" || ext == ".key" {
			id := strings.TrimSuffix(entry.Name(), ext)
			if safePart(id) {
				ids[id] = true
			}
		}
	}
	sites, err := c.certificateSites(ctx)
	if err != nil {
		return nil, err
	}
	for _, s := range sites {
		if safePart(s.Runtime.ImportedCertID) {
			ids[s.Runtime.ImportedCertID] = true
		}
	}
	out := []certChoice{}
	for id := range ids {
		r, err := c.certificateRecord(ctx, "imported:"+id)
		if err != nil {
			return nil, err
		}
		out = append(out, c.inspectCertificate(r, sites))
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Usable != out[j].Usable {
			return !out[i].Usable
		}
		if out[i].DaysLeft != out[j].DaysLeft {
			return out[i].DaysLeft < out[j].DaysLeft
		}
		return out[i].ID < out[j].ID
	})
	return out, nil
}

func (c *Core) deleteCertificate(ctx context.Context, id string, imported bool) error {
	if imported {
		id = "imported:" + id
	}
	r, err := c.certificateRecord(ctx, id)
	if err != nil {
		return err
	}
	if r.Kind != "site" && r.Kind != "imported" {
		return &AppError{Code: "CERT_DELETE_UNSUPPORTED", Message: "这里只能删除本地站点证书或导入证书"}
	}
	sites, err := c.certificateSites(ctx)
	if err != nil {
		return err
	}
	for _, s := range sites {
		if certUsed(s, r) && (s.HTTPS || imported) {
			return &AppError{Code: "CERT_IN_USE", Message: "证书仍被站点使用：" + s.Name}
		}
	}
	paths := []string{string(r.CertPath), string(r.KeyPath)}
	material := make([][]byte, 2)
	exists := make([]bool, 2)
	for i, path := range paths {
		data, err := c.readCertFile(path)
		if err != nil && !os.IsNotExist(err) {
			return err
		}
		material[i] = data
		exists[i] = err == nil
	}
	backupRoot := filepath.Join(c.Home, "backup", "certificates")
	if err = managedDirectory(c.Home, backupRoot); err != nil {
		return err
	}
	backup, err := os.MkdirTemp(backupRoot, "deleted-")
	if err != nil {
		return err
	}
	meta, _ := json.Marshal(map[string]any{"record": r, "paths": paths, "existed": exists})
	if err = atomicWrite(filepath.Join(backup, "restore.json"), meta); err != nil {
		return err
	}
	for i, data := range material {
		if exists[i] {
			if err = atomicWrite(filepath.Join(backup, fmt.Sprintf("%d.bak", i)), data); err != nil {
				return err
			}
		}
	}
	removed := 0
	operation := func() error {
		for i, path := range paths {
			if exists[i] {
				current, err := c.readCertFile(path)
				if err != nil {
					return err
				}
				if !bytes.Equal(current, material[i]) {
					return invalid("证书文件已变化，未删除")
				}
				if err = os.Remove(path); err != nil {
					return err
				}
			}
			removed++
		}
		if !imported {
			_, err := c.Store.db.ExecContext(ctx, "DELETE FROM certs WHERE id=?", r.ID)
			return err
		}
		return nil
	}()
	if operation != nil {
		for i := 0; i < removed; i++ {
			if exists[i] {
				operation = errors.Join(operation, atomicWrite(paths[i], material[i]))
			}
		}
	}
	return operation
}

func (c *Core) exportCertificate(ctx context.Context, id, outPath, format, password string) (Path, error) {
	r, err := c.certificateRecord(ctx, id)
	if err != nil {
		return "", err
	}
	certPEM, err := c.readCertFile(string(r.CertPath))
	if err != nil {
		return "", err
	}
	chain, err := parseCertChain(certPEM)
	if err != nil {
		return "", err
	}
	data := chain[0].Raw
	if format != "der" {
		keyPEM, err := c.readCertFile(string(r.KeyPath))
		if err != nil {
			return "", err
		}
		_, key, err := certPair(certPEM, keyPEM)
		if err != nil {
			return "", err
		}
		switch format {
		case "pem":
			data = append(append(append([]byte{}, certPEM...), '\n'), keyPEM...)
		case "pfx":
			data, err = buildPFX(chain, key, password)
		case "jks":
			data, err = buildJKS(chain, key, password, r.Subject)
		default:
			return "", invalid("证书导出格式无效")
		}
		if err != nil {
			return "", err
		}
	}
	if !filepath.IsAbs(outPath) {
		return "", invalid("请选择导出文件的绝对路径")
	}
	parent, err := filepath.EvalSymlinks(filepath.Dir(outPath))
	if err != nil {
		return "", err
	}
	target := filepath.Join(parent, filepath.Base(outPath))
	managed, err := filepath.EvalSymlinks(filepath.Join(c.Home, "certs"))
	if err != nil {
		return "", err
	}
	if rel, err := filepath.Rel(managed, target); err == nil && filepath.IsLocal(rel) {
		return "", invalid("不能导出到托管证书目录")
	}
	// Exclusive publication also prevents overwriting an existing hardlink or
	// symlink to a live certificate or key outside the managed directory.
	f, err := os.OpenFile(target, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return "", err
	}
	_, writeErr := f.Write(data)
	syncErr := f.Sync()
	closeErr := f.Close()
	if err = errors.Join(writeErr, syncErr, closeErr); err != nil {
		return "", &AppError{Code: "CERT_EXPORT_FAILED", Message: "导出未完成，请检查目标文件", Detail: err.Error()}
	}
	return Path(target), nil
}

func (c *Core) registerCertificates() {
	c.handle("list_certs", func(ctx context.Context, _ json.RawMessage) (any, error) {
		if !c.certChanges.TryLock() {
			return nil, serviceBusy()
		}
		defer c.certChanges.Unlock()
		return c.listCertificates(ctx)
	})
	c.handle("issue_cert", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Domain string   `json:"domain"`
			SANs   []string `json:"sans"`
		}](data)
		if err != nil {
			return nil, err
		}
		unlock, err := c.certLock()
		if err != nil {
			return nil, err
		}
		defer unlock()
		return c.issueCertificate(ctx, append([]string{in.Domain}, in.SANs...))
	})
	c.handle("trust_ca", func(ctx context.Context, _ json.RawMessage) (any, error) {
		unlock, err := c.certLock()
		if err != nil {
			return nil, err
		}
		defer unlock()
		ca, _, err := c.loadCA()
		if err != nil {
			return nil, err
		}
		if err = trustCertificate(ctx, ca); err != nil {
			return nil, err
		}
		if !certificateTrusted(ca) {
			return nil, invalid("系统尚未确认当前根 CA 受信任")
		}
		return true, nil
	})
	c.handle("cert_imported_list", func(ctx context.Context, _ json.RawMessage) (any, error) {
		if !c.certChanges.TryLock() {
			return nil, serviceBusy()
		}
		defer c.certChanges.Unlock()
		return c.importedCertificates(ctx)
	})
	c.handle("cert_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.importCertificateCommand(ctx, data, false)
	})
	c.handle("cert_imported_replace", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.importCertificateCommand(ctx, data, true)
	})
	c.handle("delete_local_cert", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		unlock, err := c.certLock()
		if err != nil {
			return nil, err
		}
		defer unlock()
		return true, c.deleteCertificate(ctx, in.ID, false)
	})
	c.handle("cert_imported_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Path string `json:"certPath"`
		}](data)
		if err != nil {
			return nil, err
		}
		unlock, err := c.certLock()
		if err != nil {
			return nil, err
		}
		defer unlock()
		id := strings.TrimSuffix(filepath.Base(in.Path), ".crt")
		if filepath.Clean(in.Path) != filepath.Join(c.Home, "certs", "imported", id+".crt") {
			return nil, invalid("请选择托管导入证书")
		}
		return true, c.deleteCertificate(ctx, id, true)
	})
	c.handle("cert_export_der", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.exportCertificateCommand(ctx, data, "der")
	})
	c.handle("cert_export_pem", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.exportCertificateCommand(ctx, data, "pem")
	})
	c.handle("cert_export_pfx", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.exportCertificateCommand(ctx, data, "pfx")
	})
	c.handle("cert_export_jks", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.exportCertificateCommand(ctx, data, "jks")
	})
	c.handle("site_certificate_choices", func(ctx context.Context, _ json.RawMessage) (any, error) {
		if !c.certChanges.TryLock() {
			return nil, serviceBusy()
		}
		defer c.certChanges.Unlock()
		out, err := c.importedCertificates(ctx)
		if err != nil {
			return nil, err
		}
		records, err := c.certificateRecords(ctx)
		if err != nil {
			return nil, err
		}
		sites, err := c.certificateSites(ctx)
		if err != nil {
			return nil, err
		}
		for _, r := range records {
			if r.Kind == "acme" && len(r.SANs) > 0 && r.SANs[0] == r.Subject {
				out = append(out, c.inspectCertificate(r, sites))
			}
		}
		return out, nil
	})
}
func (c *Core) importCertificateCommand(ctx context.Context, data json.RawMessage, replace bool) (any, error) {
	in, err := decode[struct {
		ID       string `json:"id"`
		CertPath string `json:"certPath"`
		KeyPath  string `json:"keyPath"`
	}](data)
	if err != nil {
		return nil, err
	}
	unlock, err := c.certLock()
	if err != nil {
		return nil, err
	}
	defer unlock()
	if replace {
		if !safePart(in.ID) {
			return nil, invalid("请选择要更新的证书")
		}
		list, err := c.importedCertificates(ctx)
		if err != nil {
			return nil, err
		}
		found := false
		for _, r := range list {
			found = found || r.ID == in.ID
		}
		if !found {
			return nil, invalid("导入证书已不存在")
		}
	} else {
		in.ID = ""
	}
	return c.importCertificate(ctx, in.CertPath, in.KeyPath, in.ID)
}
func (c *Core) exportCertificateCommand(ctx context.Context, data json.RawMessage, format string) (any, error) {
	in, err := decode[struct {
		ID       string `json:"certId"`
		Path     string `json:"outPath"`
		Password string `json:"password"`
	}](data)
	if err != nil {
		return nil, err
	}
	unlock, err := c.certLock()
	if err != nil {
		return nil, err
	}
	defer unlock()
	return c.exportCertificate(ctx, in.ID, in.Path, format, in.Password)
}
