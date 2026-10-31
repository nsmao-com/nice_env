package core

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

type certHealth struct {
	ID          string   `json:"id"`
	Kind        string   `json:"kind"`
	Subject     string   `json:"subject"`
	SANs        []string `json:"sans"`
	NotAfter    int64    `json:"notAfter"`
	DaysLeft    int64    `json:"daysLeft"`
	Status      string   `json:"status"`
	FilePresent bool     `json:"filePresent"`
	UsedBySites []string `json:"usedBySites"`
	MissingSANs []string `json:"missingSans"`
	Advice      string   `json:"advice"`
}

func (c *Core) certHealth(ctx context.Context) (any, error) {
	if !c.certChanges.TryLock() {
		return nil, serviceBusy()
	}
	defer c.certChanges.Unlock()
	records, err := c.certificateRecords(ctx)
	if err != nil {
		return nil, err
	}
	sites, err := c.certificateSites(ctx)
	if err != nil {
		return nil, err
	}
	expectedCA := false
	outRecords := []certRecord{}
	for _, r := range records {
		expectedCA = expectedCA || r.Kind == "ca" || r.Kind == "site"
		if r.Kind != "ca" {
			outRecords = append(outRecords, r)
		}
	}
	for _, name := range []string{"ca.crt", "ca.key"} {
		_, err := os.Lstat(filepath.Join(c.Home, "certs", name))
		expectedCA = expectedCA || err == nil
	}
	for _, s := range sites {
		expectedCA = expectedCA || s.HTTPS && s.Runtime.ACMECertID == "" && s.Runtime.ImportedCertID == ""
	}
	if expectedCA {
		outRecords = append(outRecords, certRecord{ID: "ca", Kind: "ca", Subject: "NiceEnv Local Root CA", CertPath: Path(filepath.Join(c.Home, "certs", "ca.crt")), KeyPath: Path(filepath.Join(c.Home, "certs", "ca.key"))})
	}
	imports, err := c.importedCertificates(ctx)
	if err != nil {
		return nil, err
	}
	choices := []certChoice{}
	for _, r := range outRecords {
		choices = append(choices, c.inspectCertificate(r, sites))
	}
	choices = append(choices, imports...)
	out := []certHealth{}
	expired, critical, warning := 0, 0, 0
	for _, choice := range choices {
		h := certHealth{ID: choice.ID, Kind: choice.Kind, Subject: choice.Subject, SANs: choice.SANs, NotAfter: choice.NotAfter, DaysLeft: choice.DaysLeft, Status: "ok", UsedBySites: choice.UsedBySites, MissingSANs: []string{}, Advice: choice.Problem}
		certPEM, certErr := c.readCertFile(string(choice.CertPath))
		_, keyErr := c.readCertFile(string(choice.KeyPath))
		h.FilePresent = certErr == nil && keyErr == nil
		if chain, err := parseCertChain(certPEM); err == nil {
			seen := map[string]bool{}
			r := certRecord{ID: choice.ID, Kind: choice.Kind, Subject: choice.Subject}
			for _, s := range sites {
				if s.HTTPS && certUsed(s, r) {
					for _, domain := range s.Domains {
						if !certCovers(chain[0], domain) && !seen[domain] {
							h.MissingSANs = append(h.MissingSANs, domain)
							seen[domain] = true
						}
					}
				}
			}
			sort.Strings(h.MissingSANs)
		}
		switch {
		case choice.NotAfter > 0 && choice.NotAfter <= time.Now().Unix():
			h.Status = "expired"
			expired++
		case !choice.Usable:
			h.Status = "invalid"
			critical++
		case choice.DaysLeft <= 7:
			h.Status = "critical"
			critical++
		case choice.DaysLeft <= 30 && choice.Kind != "site":
			h.Status = "warn"
			warning++
		}
		if h.Advice == "" && h.Status != "ok" {
			h.Advice = "证书即将到期，请续签并更新使用此证书的站点。"
		}
		out = append(out, h)
	}
	sort.Slice(out, func(i, j int) bool {
		if (out[i].Advice == "") != (out[j].Advice == "") {
			return out[i].Advice != ""
		}
		return out[i].DaysLeft < out[j].DaysLeft
	})
	trusted := false
	if ca, _, err := c.loadCA(); err == nil {
		trusted = certificateTrusted(ca)
	}
	return map[string]any{"certs": out, "expired": expired, "critical": critical, "warning": warning, "caTrusted": trusted, "checkedAt": time.Now().Unix()}, nil
}

func (c *Core) repairCertificates(ctx context.Context) ([]string, error) {
	sites, err := c.certificateSites(ctx)
	if err != nil {
		return nil, err
	}
	groups := map[string][]string{}
	order := []string{}
	for _, s := range sites {
		if s.HTTPS && s.Runtime.ImportedCertID == "" && s.Runtime.ACMECertID == "" && len(s.Domains) > 0 {
			primary := s.Domains[0]
			if _, ok := groups[primary]; !ok {
				order = append(order, primary)
			}
			groups[primary] = append(groups[primary], s.Domains...)
		}
	}
	sort.Strings(order)
	reissued := []string{}
	for _, primary := range order {
		domains, err := normalizeCertDomains(groups[primary])
		if err != nil {
			return nil, err
		}
		certPEM, certErr := c.readCertFile(filepath.Join(c.Home, "certs", "sites", certStem(primary)+".crt"))
		keyPEM, keyErr := c.readCertFile(filepath.Join(c.Home, "certs", "sites", certStem(primary)+".key"))
		valid := false
		if certErr == nil && keyErr == nil {
			if chain, _, err := certPair(certPEM, keyPEM); err == nil {
				ca, _, err := c.loadCA()
				leaf := chain[0]
				valid = err == nil && leaf.CheckSignatureFrom(ca) == nil && certValidity(leaf, true) == nil && time.Until(leaf.NotAfter) > 7*24*time.Hour
				for _, domain := range domains {
					valid = valid && certCovers(leaf, domain)
				}
			}
		}
		if !valid {
			if _, err = c.issueCertificate(ctx, domains); err != nil {
				return nil, &AppError{Code: "CERT_REPAIR_INCOMPLETE", Message: "部分站点证书未能续签", Detail: publicError(err).Message, Hint: "已完成：" + strings.Join(reissued, "、")}
			}
			reissued = append(reissued, primary)
		}
	}
	return reissued, nil
}

func (c *Core) importCertificateDirectory(ctx context.Context, dir string) (any, error) {
	if !filepath.IsAbs(dir) {
		return nil, invalid("请选择证书文件夹的绝对路径")
	}
	root, err := os.OpenRoot(dir)
	if err != nil {
		return nil, err
	}
	defer root.Close()
	groups := map[string][]string{}
	count := 0
	var walk func(string, int) error
	walk = func(relative string, depth int) error {
		if err := ctx.Err(); err != nil {
			return err
		}
		f, err := root.Open(relative)
		if err != nil {
			return err
		}
		entries, readErr := f.ReadDir(5001)
		f.Close()
		if readErr != nil && !errors.Is(readErr, io.EOF) {
			return readErr
		}
		for _, entry := range entries {
			count++
			if count > 5000 {
				return invalid("证书目录超过 5000 个条目，请选择更小的目录")
			}
			if entry.Type()&(os.ModeSymlink|os.ModeIrregular) != 0 {
				continue
			}
			name := filepath.Join(relative, entry.Name())
			if entry.IsDir() {
				if depth < 3 {
					if err := walk(name, depth+1); err != nil {
						return err
					}
				}
				continue
			}
			groups[filepath.Dir(name)] = append(groups[filepath.Dir(name)], entry.Name())
		}
		return nil
	}
	if err = walk(".", 0); err != nil {
		return nil, err
	}
	imported := []certChoice{}
	skipped := []string{}
	directories := []string{}
	for directory := range groups {
		directories = append(directories, directory)
	}
	sort.Strings(directories)
	for _, directory := range directories {
		names := groups[directory]
		sort.Strings(names)
		present := map[string]bool{}
		for _, name := range names {
			present[name] = true
		}
		for _, name := range names {
			ext := strings.ToLower(filepath.Ext(name))
			if ext != ".crt" && ext != ".cer" && ext != ".pem" {
				continue
			}
			stem := strings.TrimSuffix(name, filepath.Ext(name))
			candidates := []string{stem + ".key"}
			if name == "fullchain.pem" || name == "cert.pem" {
				candidates = append(candidates, "privkey.pem")
			}
			if name == "certificate.crt" {
				candidates = append(candidates, "private.key")
			}
			key := ""
			for _, candidate := range candidates {
				if present[candidate] {
					key = candidate
					break
				}
			}
			if key == "" {
				skipped = append(skipped, filepath.ToSlash(filepath.Join(directory, name))+"：未找到匹配私钥")
				continue
			}
			for _, candidate := range []string{name, key} {
				if _, err = siteCheck(root, filepath.Join(directory, candidate), false); err != nil {
					return nil, err
				}
			}
			entry, err := c.importCertificate(ctx, filepath.Join(dir, directory, name), filepath.Join(dir, directory, key), "")
			if err != nil {
				skipped = append(skipped, filepath.ToSlash(filepath.Join(directory, name))+"："+publicError(err).Message)
			} else {
				imported = append(imported, entry)
			}
		}
	}
	return map[string]any{"imported": imported, "skipped": skipped}, nil
}
func (c *Core) registerCertificateHealth() {
	c.handle("cert_health", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.certHealth(ctx) })
	c.handle("reissue_site_certs", func(ctx context.Context, _ json.RawMessage) (any, error) {
		unlock, err := c.certLock()
		if err != nil {
			return nil, err
		}
		defer unlock()
		return c.repairCertificates(ctx)
	})
	c.handle("cert_import_dir", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Dir string `json:"dir"`
		}](data)
		if err != nil {
			return nil, err
		}
		unlock, err := c.certLock()
		if err != nil {
			return nil, err
		}
		defer unlock()
		return c.importCertificateDirectory(ctx, in.Dir)
	})
}
