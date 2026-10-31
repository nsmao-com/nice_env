package core

import (
	"context"
	"crypto"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"encoding/pem"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/go-acme/lego/v5/acme"
	"github.com/go-acme/lego/v5/certcrypto"
	"github.com/go-acme/lego/v5/certificate"
	"github.com/go-acme/lego/v5/challenge"
	"github.com/go-acme/lego/v5/challenge/dns01"
	"github.com/go-acme/lego/v5/lego"
	"github.com/go-acme/lego/v5/providers/dns/alidns"
	"github.com/go-acme/lego/v5/providers/dns/cloudflare"
	"github.com/go-acme/lego/v5/providers/dns/digitalocean"
	"github.com/go-acme/lego/v5/providers/dns/godaddy"
	"github.com/go-acme/lego/v5/providers/dns/huaweicloud"
	"github.com/go-acme/lego/v5/providers/dns/porkbun"
	"github.com/go-acme/lego/v5/registration"
	"niceenv/internal/process"
)

type certIssuedMaterial struct {
	Version      int      `json:"version"`
	AutomationID string   `json:"automation_id"`
	DeploymentID string   `json:"deployment_id"`
	Domains      []string `json:"domains"`
	CA           string   `json:"ca"`
	KeyAlg       string   `json:"key_alg"`
	Chain        string   `json:"chain"`
	KeyPEM       string   `json:"key_pem"`
}
type certACMEUser struct {
	email        string
	key          crypto.Signer
	registration *acme.ExtendedAccount
}

func (u *certACMEUser) GetEmail() string                       { return u.email }
func (u *certACMEUser) GetPrivateKey() crypto.Signer           { return u.key }
func (u *certACMEUser) GetRegistration() *acme.ExtendedAccount { return u.registration }

func writePrivateAtomic(path string, body []byte) error {
	f, err := os.CreateTemp(filepath.Dir(path), ".niceenv-private-")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	if err = process.PrivateFile(f.Name()); err == nil {
		_, err = f.Write(body)
	}
	err = errors.Join(err, f.Sync(), f.Close())
	if err != nil {
		return err
	}
	return os.Rename(f.Name(), path)
}
func readPrivateMaterial(path string) ([]byte, error) {
	return readPrivateMaterialLimit(path, 8*1024*1024)
}

func readPrivateMaterialLimit(path string, limit int64) ([]byte, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Size() > limit {
		return nil, invalid("私有数据文件不是普通文件或超过大小限制")
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	body, err := io.ReadAll(io.LimitReader(f, limit+1))
	if int64(len(body)) > limit {
		return nil, invalid("私有数据文件过大")
	}
	return body, err
}
func (c *Core) certACMEAccount(a certAutomation) (crypto.Signer, error) {
	dir := filepath.Join(c.Home, "certs", "acme")
	if err := managedDirectory(c.Home, dir); err != nil {
		return nil, err
	}
	path := filepath.Join(dir, a.ID+".account.key")
	body, err := readPrivateMaterial(path)
	if err == nil {
		return certcrypto.ParsePEMPrivateKey(body)
	}
	if !os.IsNotExist(err) {
		return nil, err
	}
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, err
	}
	raw, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		return nil, err
	}
	if err = writePrivateAtomic(path, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: raw})); err != nil {
		return nil, err
	}
	return key, nil
}

func certDNSProvider(a certAutomation, client *http.Client) (challenge.Provider, error) {
	const timeout = 10 * time.Minute
	const interval = 5 * time.Second
	switch a.DNS.Kind {
	case "manual":
		return nil, nil
	case "cloudflare":
		return cloudflare.NewDNSProviderConfig(&cloudflare.Config{AuthToken: a.DNS.AccessKey, ZoneToken: a.DNS.AccessKey, BaseURL: "https://api.cloudflare.com/client/v4", TTL: 120, PropagationTimeout: timeout, PollingInterval: interval, HTTPClient: client})
	case "digitalocean":
		return digitalocean.NewDNSProviderConfig(&digitalocean.Config{AuthToken: a.DNS.AccessKey, BaseURL: "https://api.digitalocean.com", TTL: 30, PropagationTimeout: timeout, PollingInterval: interval, HTTPClient: client})
	case "porkbun":
		return porkbun.NewDNSProviderConfig(&porkbun.Config{APIKey: a.DNS.AccessKey, SecretAPIKey: a.DNS.Secret, TTL: 600, PropagationTimeout: timeout, PollingInterval: interval, HTTPClient: client})
	case "godaddy":
		return godaddy.NewDNSProviderConfig(&godaddy.Config{APIKey: a.DNS.AccessKey, APISecret: a.DNS.Secret, TTL: 600, PropagationTimeout: timeout, PollingInterval: interval, HTTPClient: client})
	case "aliyun":
		return alidns.NewDNSProviderConfig(&alidns.Config{APIKey: a.DNS.AccessKey, SecretKey: a.DNS.Secret, RegionID: "cn-hangzhou", TTL: 600, PropagationTimeout: timeout, PollingInterval: interval, HTTPTimeout: 30 * time.Second})
	case "huawei":
		return huaweicloud.NewDNSProviderConfig(&huaweicloud.Config{AccessKeyID: a.DNS.AccessKey, SecretAccessKey: a.DNS.Secret, Region: "cn-north-4", TTL: 300, PropagationTimeout: timeout, PollingInterval: interval, HTTPTimeout: 30 * time.Second})
	case "dnspod":
		return &legacyDNSPod{client: client, token: a.DNS.AccessKey + "," + a.DNS.Secret, records: map[string]dnsPodRecord{}}, nil
	}
	return nil, invalid("DNS 服务商无效")
}

type certChallengeProvider struct {
	core       *Core
	a          *certAutomation
	provider   challenge.Provider
	mu         sync.Mutex
	cleanupErr error
}

func (p *certChallengeProvider) Timeout() (time.Duration, time.Duration) {
	return time.Duration(max(600, p.a.DNSWaitSec+600)) * time.Second, 5 * time.Second
}
func (p *certChallengeProvider) Present(ctx context.Context, domain, token, keyAuth string) error {
	info := dns01.GetChallengeInfo(ctx, domain, keyAuth)
	if p.a.CNAMETarget != "" {
		expected := strings.ReplaceAll(strings.TrimSuffix(p.a.CNAMETarget, "."), "{domain}", strings.TrimPrefix(domain, "*."))
		if !strings.EqualFold(strings.TrimSuffix(info.EffectiveFQDN, "."), "_acme-challenge."+expected) {
			return invalid("CNAME 验证记录尚未指向配置的授权域名")
		}
	}
	p.mu.Lock()
	p.a.ManualRecords = append(p.a.ManualRecords, certTXT{Name: strings.TrimSuffix(info.EffectiveFQDN, "."), Value: info.Value})
	p.a.State = "issuing"
	if p.provider == nil {
		p.a.State = "manual_wait"
	}
	err := p.core.saveCertAutomation(ctx, p.a)
	p.mu.Unlock()
	if err != nil {
		return err
	}
	if p.provider != nil {
		if err = p.provider.Present(ctx, domain, token, keyAuth); err != nil {
			return err
		}
	}
	if p.a.DNSWaitSec > 0 {
		timer := time.NewTimer(time.Duration(p.a.DNSWaitSec) * time.Second)
		defer timer.Stop()
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timer.C:
		}
	}
	return nil
}
func (p *certChallengeProvider) CleanUp(_ context.Context, domain, token, keyAuth string) error {
	// Cancellation must not skip removal of records already created by this run.
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	var err error
	if p.provider != nil {
		err = p.provider.CleanUp(ctx, domain, token, keyAuth)
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	p.cleanupErr = errors.Join(p.cleanupErr, err)
	// Keep manual TXT values visible after the run so users can remove them.
	if err == nil && p.provider != nil {
		info := dns01.GetChallengeInfo(ctx, domain, keyAuth)
		rows := p.a.ManualRecords[:0]
		for _, row := range p.a.ManualRecords {
			if row.Value != info.Value || !strings.EqualFold(row.Name, strings.TrimSuffix(info.EffectiveFQDN, ".")) {
				rows = append(rows, row)
			}
		}
		p.a.ManualRecords = rows
	}
	return errors.Join(err, p.core.saveCertAutomation(ctx, p.a))
}

func (c *Core) issueACMEMaterial(ctx context.Context, a *certAutomation) (certIssuedMaterial, error) {
	var material certIssuedMaterial
	key, err := c.certACMEAccount(*a)
	if err != nil {
		return material, err
	}
	transport := outboundTransport()
	defer transport.CloseIdleConnections()
	httpClient := &http.Client{Transport: transport, Timeout: 30 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	provider, err := certDNSProvider(*a, httpClient)
	if err != nil {
		return material, err
	}
	user := &certACMEUser{email: a.Email, key: key}
	directory := map[string]string{"letsencrypt": lego.DirectoryURLLetsEncrypt, "letsencrypt-staging": lego.DirectoryURLLetsEncryptStaging, "zerossl": "https://acme.zerossl.com/v2/DV90", "google": "https://dv.acme-v02.api.pki.goog/directory", "buypass": "https://api.buypass.com/acme/directory"}[a.CA]
	config := &lego.Config{CADirURL: directory, User: user, UserAgent: "NiceEnv/" + c.Version, HTTPClient: httpClient, Certificate: lego.CertificateConfig{Timeout: 2 * time.Minute, OverallRequestLimit: 18}}
	client, err := lego.NewClient(config)
	if err != nil {
		return material, err
	}
	if a.EABKID != "" {
		user.registration, err = client.Registration.RegisterWithExternalAccountBinding(ctx, registration.RegisterEABOptions{TermsOfServiceAgreed: true, Kid: a.EABKID, HmacEncoded: a.EABHMACKey})
	} else {
		user.registration, err = client.Registration.Register(ctx, registration.RegisterOptions{TermsOfServiceAgreed: true})
	}
	if err != nil {
		return material, err
	}
	wrapper := &certChallengeProvider{core: c, a: a, provider: provider}
	if err = client.Challenge.SetDNS01Provider(wrapper); err != nil {
		return material, err
	}
	keyType := map[string]certcrypto.KeyType{"ec256": certcrypto.EC256, "ec384": certcrypto.EC384, "rsa2048": certcrypto.RSA2048, "rsa3072": certcrypto.RSA3072, "rsa4096": certcrypto.RSA4096}[a.KeyAlg]
	resource, err := client.Certificate.Obtain(ctx, certificate.ObtainRequest{Domains: a.Domains, KeyType: keyType, Bundle: true})
	if err != nil {
		return material, errors.Join(err, wrapper.cleanupErr)
	}
	batch, err := newID("issued")
	if err != nil {
		return material, err
	}
	material = certIssuedMaterial{Version: 1, AutomationID: a.ID, DeploymentID: batch, Domains: a.Domains, CA: a.CA, KeyAlg: a.KeyAlg, Chain: string(resource.Certificate), KeyPEM: string(resource.PrivateKey)}
	return material, wrapper.cleanupErr
}
