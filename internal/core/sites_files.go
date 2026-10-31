package core

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha1"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"golang.org/x/crypto/bcrypt"
	"niceenv/internal/process"
)

// The journal retains original bytes before publishing any site change. Rollback
// compares the applied bytes so an unrelated edit is never silently overwritten.
type siteFileChange struct {
	path                      string
	before, after             []byte
	existed, present, applied bool
}
type siteFileJournal struct {
	core       *Core
	files      []*siteFileChange
	backup     string
	webRunning []string
}

func (c *Core) newSiteJournal() *siteFileJournal {
	j := &siteFileJournal{core: c}
	c.services.mu.RLock()
	defer c.services.mu.RUnlock()
	for _, id := range []string{"nginx", "apache", "caddy"} {
		if e := c.services.entries[id]; e != nil && e.child != nil && e.status.State == "running" {
			j.webRunning = append(j.webRunning, id)
		}
	}
	return j
}

func (j *siteFileJournal) add(path string, content []byte, present bool) error {
	for _, item := range j.files {
		if item.path == path {
			item.after = content
			item.present = present
			return nil
		}
	}
	root, err := os.OpenRoot(filepath.Dir(path))
	if err != nil {
		return err
	}
	defer root.Close()
	before, exists, err := envRead(root, filepath.Base(path))
	if err != nil {
		return err
	}
	j.files = append(j.files, &siteFileChange{path: path, before: before, after: content, existed: exists, present: present})
	return nil
}
func (j *siteFileJournal) apply(ctx context.Context) error {
	if len(j.files) == 0 {
		return nil
	}
	parent := filepath.Join(j.core.Home, "backup", "sites")
	if err := managedDirectory(j.core.Home, parent); err != nil {
		return err
	}
	backup, err := os.MkdirTemp(parent, "change-")
	if err != nil {
		return err
	}
	j.backup = backup
	mapping := []map[string]any{}
	for i, item := range j.files {
		name := fmt.Sprintf("%d.bak", i)
		mapping = append(mapping, map[string]any{"path": Path(item.path), "original": name, "existed": item.existed})
		if item.existed {
			if err = os.WriteFile(filepath.Join(backup, name), item.before, 0600); err != nil {
				return err
			}
		}
	}
	encoded, err := json.MarshalIndent(mapping, "", "  ")
	if err != nil {
		return err
	}
	if err = os.WriteFile(filepath.Join(backup, "files.json"), encoded, 0600); err != nil {
		return err
	}
	for _, item := range j.files {
		if err = ctx.Err(); err != nil {
			return err
		}
		if item.existed == item.present && bytes.Equal(item.before, item.after) {
			continue
		}
		if err = siteFileReplace(item.path, item.before, item.existed, item.after, item.present); err != nil {
			return err
		}
		item.applied = true
	}
	return nil
}
func siteFileReplace(path string, before []byte, existed bool, after []byte, present bool) error {
	root, err := os.OpenRoot(filepath.Dir(path))
	if err != nil {
		return err
	}
	defer root.Close()
	name := filepath.Base(path)
	if present {
		return replaceEnv(root, name, before, existed, after)
	}
	current, exists, err := envRead(root, name)
	if err != nil {
		return err
	}
	if exists != existed || !bytes.Equal(current, before) {
		return envChanged()
	}
	if exists {
		return root.Remove(name)
	}
	return nil
}
func (j *siteFileJournal) restore() error {
	failures := []error{}
	for i := len(j.files) - 1; i >= 0; i-- {
		item := j.files[i]
		if item.applied {
			if err := siteFileReplace(item.path, item.after, item.present, item.before, item.existed); err != nil {
				failures = append(failures, fmt.Errorf("%s: %w", DisplayPath(item.path), err))
			}
		}
	}
	return errors.Join(failures...)
}

func (c *Core) siteLock() (func(), error) {
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
	local := func() { c.certChanges.Unlock(); c.services.lifecycle.Unlock(); c.siteChanges.Unlock() }
	dir := filepath.Join(c.Home, "service-locks")
	if err := managedDirectory(c.Home, dir); err != nil {
		local()
		return nil, err
	}
	siteUnlock, err := process.Lock(filepath.Join(dir, "sites.lock"))
	if err != nil {
		local()
		return nil, err
	}
	certUnlock, err := process.Lock(filepath.Join(dir, "certificates.lock"))
	if err != nil {
		siteUnlock()
		local()
		return nil, err
	}
	return func() { certUnlock(); siteUnlock(); local() }, nil
}

func (c *Core) siteAuthPath(id string) string {
	return filepath.Join(c.Home, "etc", "auth", id+".htpasswd")
}
func (c *Core) siteAuth(site *Site, previous *Site) (string, error) {
	auth := site.Runtime.BasicAuth
	if auth == nil || !auth.Enabled {
		if auth != nil {
			auth.Password = nil
			auth.HasPassword = false
		}
		return "", nil
	}
	var hash string
	if auth.Password != nil {
		// Nginx implements RFC 2307 SSHA on every platform; its Windows build
		// does not provide crypt(3) bcrypt. Keep bcrypt for Apache/Caddy and a
		// separately salted Nginx sidecar, never a plaintext password.
		salt := make([]byte, 24)
		if _, err := rand.Read(salt); err != nil {
			return "", err
		}
		digest := sha1.New()
		digest.Write([]byte(*auth.Password))
		digest.Write(salt)
		auth.nginxHash = "{SSHA}" + base64.StdEncoding.EncodeToString(append(digest.Sum(nil), salt...))
		value, err := bcrypt.GenerateFromPassword([]byte(*auth.Password), bcrypt.DefaultCost)
		if err != nil {
			return "", err
		}
		hash = string(value)
	} else {
		text, err := readConfigText(c.siteAuthPath(site.ID))
		if os.IsNotExist(err) {
			return "", &AppError{Code: "BASIC_AUTH_PASSWORD_REQUIRED", Message: "启用 Basic Auth 时请填写密码；已有密码可以留空保留"}
		}
		if err != nil {
			return "", err
		}
		username, stored, ok := strings.Cut(strings.TrimSpace(text), ":")
		expected := auth.Username
		if previous != nil && previous.Runtime.BasicAuth != nil {
			expected = previous.Runtime.BasicAuth.Username
		}
		if !ok || username != expected || len(stored) != 60 {
			return "", invalid("Basic Auth 文件与站点记录不一致")
		}
		if _, err = bcrypt.Cost([]byte(stored)); err != nil {
			return "", invalid("Basic Auth 密码哈希无效")
		}
		hash = stored
	}
	auth.Password = nil
	auth.HasPassword = true
	return hash, nil
}

func (c *Core) nginxSiteAuth(site Site, previous *Site) (string, error) {
	auth := site.Runtime.BasicAuth
	if auth == nil || !auth.Enabled {
		return "", nil
	}
	if auth.nginxHash != "" {
		return auth.nginxHash, nil
	}
	text, err := readConfigText(c.siteAuthPath(site.ID) + ".nginx")
	if os.IsNotExist(err) {
		return "", &AppError{Code: "BASIC_AUTH_PASSWORD_REQUIRED", Message: "此站点的旧密码哈希不兼容 Windows Nginx，请重新输入一次 Basic Auth 密码", Hint: "旧认证文件已保留；保存后会生成兼容文件，不保存明文密码。"}
	}
	if err != nil {
		return "", err
	}
	username, hash, ok := strings.Cut(strings.TrimSpace(text), ":")
	expected := auth.Username
	if previous != nil && previous.Runtime.BasicAuth != nil {
		expected = previous.Runtime.BasicAuth.Username
	}
	value, err := base64.StdEncoding.DecodeString(strings.TrimPrefix(hash, "{SSHA}"))
	if !ok || username != expected || !strings.HasPrefix(hash, "{SSHA}") || err != nil || len(value) < 36 {
		return "", invalid("Nginx 认证文件无效，请重新设置密码")
	}
	return hash, nil
}

func (c *Core) siteCertificate(ctx context.Context, site Site, create bool) (certRecord, error) {
	if !site.HTTPS {
		return certRecord{}, nil
	}
	id := "cert-" + site.Domains[0]
	if site.Runtime.ImportedCertID != nil {
		id = "imported:" + *site.Runtime.ImportedCertID
	}
	if site.Runtime.ACMECertID != nil {
		id = *site.Runtime.ACMECertID
		if !strings.HasPrefix(id, "acme-") {
			return certRecord{}, invalid("ACME 证书标识无效")
		}
	}
	record, err := c.certificateRecord(ctx, id)
	valid := func() error {
		if err != nil {
			return err
		}
		cert, err := c.readCertFile(string(record.CertPath))
		if err != nil {
			return err
		}
		key, err := c.readCertFile(string(record.KeyPath))
		if err != nil {
			return err
		}
		chain, _, err := certPair(cert, key)
		if err != nil {
			return err
		}
		if err = certValidity(chain[0], true); err != nil {
			return err
		}
		for _, domain := range site.Domains {
			if !certCovers(chain[0], domain) {
				return invalid("证书未覆盖站点域名：" + domain)
			}
		}
		return nil
	}
	if validation := valid(); validation != nil {
		if !create || site.Runtime.ImportedCertID != nil || site.Runtime.ACMECertID != nil {
			return certRecord{}, validation
		}
		return c.issueCertificate(ctx, site.Domains)
	}
	return record, nil
}

func sanitizedSite(site Site) Site {
	if site.Runtime.BasicAuth != nil {
		auth := *site.Runtime.BasicAuth
		auth.Password = nil
		site.Runtime.BasicAuth = &auth
	}
	if site.DB != nil {
		db := *site.DB
		db.Password = ""
		site.DB = &db
	}
	return site
}
func (c *Core) saveSite(ctx context.Context, site Site, create bool) error {
	site = sanitizedSite(site)
	values := []any{site.ID, site.Name}
	domains, err := json.Marshal(site.Domains)
	if err != nil {
		return err
	}
	rt, err := json.Marshal(site.Runtime)
	if err != nil {
		return err
	}
	rewrite, err := json.Marshal(site.Rewrite)
	if err != nil {
		return err
	}
	db, err := json.Marshal(site.DB)
	if err != nil {
		return err
	}
	overrides, err := json.Marshal(site.PHPOverrides)
	if err != nil {
		return err
	}
	values = append(values, string(domains), DisplayPath(string(site.RootDir)), string(rt), site.HTTPS, string(rewrite), string(db), string(overrides), site.CreatedAt, site.UpdatedAt)
	query := `INSERT INTO sites(id,name,domains,root_dir,runtime,https,rewrite,db,php_overrides,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)`
	if !create {
		query += ` ON CONFLICT(id) DO UPDATE SET name=excluded.name,domains=excluded.domains,root_dir=excluded.root_dir,runtime=excluded.runtime,https=excluded.https,rewrite=excluded.rewrite,db=excluded.db,php_overrides=excluded.php_overrides,updated_at=excluded.updated_at`
	}
	_, err = c.Store.db.ExecContext(ctx, query, values...)
	return err
}

func (c *Core) siteRollback(err error, journal *siteFileJournal, restore func(context.Context) error) error {
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	fileErr := journal.restore()
	restoreErr := restore(ctx)
	if fileErr == nil {
		for _, id := range journal.webRunning {
			e, lookupErr := c.serviceEntry(id, "")
			if lookupErr != nil {
				restoreErr = errors.Join(restoreErr, lookupErr)
				continue
			}
			c.services.mu.RLock()
			running := e.child != nil && e.status.State == "running"
			c.services.mu.RUnlock()
			if !running {
				restoreErr = errors.Join(restoreErr, c.startService(ctx, e))
			}
		}
	}
	if recovery := errors.Join(fileErr, restoreErr); recovery != nil {
		return &AppError{Code: "SITE_ROLLBACK_FAILED", Message: "站点操作失败，部分状态未能恢复", Detail: err.Error() + "；" + recovery.Error(), Hint: "请保留恢复目录：" + DisplayPath(journal.backup)}
	}
	return err
}
