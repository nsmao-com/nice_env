package core

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"

	"niceenv/internal/process"
)

func certOutputPath(raw string, create bool) (string, error) {
	p := DisplayPath(raw)
	if !filepath.IsAbs(p) {
		return "", invalid("证书输出须为绝对文件路径")
	}
	volume := filepath.VolumeName(p)
	rest := strings.TrimPrefix(p, volume)
	for _, part := range strings.FieldsFunc(rest, func(r rune) bool { return r == '/' || r == '\\' }) {
		if part == "." || part == ".." || part == "" || strings.ContainsAny(part, "\x00\r\n") {
			return "", invalid("证书路径包含无效分段")
		}
		if runtime.GOOS == "windows" && (!filepath.IsLocal(part) || strings.ContainsAny(part, ":*?\"<>|") || strings.HasSuffix(part, ".") || strings.HasSuffix(part, " ")) {
			return "", invalid("证书路径包含 Windows 不支持的文件名")
		}
	}
	p = filepath.Clean(p)
	parent := filepath.Dir(p)
	var chain []string
	for cur := parent; ; cur = filepath.Dir(cur) {
		chain = append(chain, cur)
		if filepath.Dir(cur) == cur {
			break
		}
	}
	for i := len(chain) - 1; i >= 0; i-- {
		cur := chain[i]
		info, err := os.Lstat(cur)
		if os.IsNotExist(err) {
			if !create {
				continue
			}
			if err = os.Mkdir(cur, 0700); err != nil {
				return "", err
			}
			continue
		}
		if err != nil {
			return "", err
		}
		if !info.IsDir() || info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return "", invalid("证书路径不能经过链接或目录联接")
		}
	}
	if info, err := os.Lstat(p); err == nil {
		if !info.Mode().IsRegular() || info.Size() > 8*1024*1024 {
			return "", invalid("证书输出必须是 8 MiB 以内的普通文件")
		}
	} else if !os.IsNotExist(err) {
		return "", err
	}
	return p, nil
}

// Back up every original before publishing any member. On failure, restore
// only bytes written by this operation, retaining backups for manual recovery.
func (c *Core) deployCertFiles(ctx context.Context, paths []string, contents [][]byte, commit func() error) error {
	if len(paths) != len(contents) {
		return invalid("证书文件数量不一致")
	}
	before := make([][]byte, len(paths))
	exists := make([]bool, len(paths))
	seen := map[string]bool{}
	for i, p := range paths {
		checked, err := certOutputPath(p, true)
		if err != nil {
			return err
		}
		paths[i] = checked
		identity := checked
		if runtime.GOOS == "windows" || runtime.GOOS == "darwin" {
			identity = strings.ToLower(identity)
		}
		if seen[identity] {
			return invalid("证书和私钥输出路径重复")
		}
		seen[identity] = true
		before[i], err = readPrivateMaterial(checked)
		if err != nil && !os.IsNotExist(err) {
			return err
		}
		exists[i] = err == nil
	}
	root := filepath.Join(c.Home, "backup", "certificates")
	if err := managedDirectory(c.Home, root); err != nil {
		return err
	}
	backup, err := os.MkdirTemp(root, "deploy-")
	if err != nil {
		return err
	}
	for i := range paths {
		if exists[i] {
			if err = writePrivateAtomic(filepath.Join(backup, fmt.Sprintf("%d.bak", i)), before[i]); err != nil {
				return err
			}
		}
	}
	meta, err := json.Marshal(map[string]any{"paths": paths, "existed": exists})
	if err != nil {
		return err
	}
	if err = writePrivateAtomic(filepath.Join(backup, "restore.json"), meta); err != nil {
		return err
	}
	written := 0
	outcome := func() error {
		for i, p := range paths {
			if err := ctx.Err(); err != nil {
				return err
			}
			if _, err := certOutputPath(p, false); err != nil {
				return err
			}
			now, err := readPrivateMaterial(p)
			if err != nil && !os.IsNotExist(err) {
				return err
			}
			if (err == nil) != exists[i] || !bytes.Equal(now, before[i]) {
				return invalid("部署文件已被外部修改")
			}
			if err = writePrivateAtomic(p, contents[i]); err != nil {
				return err
			}
			written++
		}
		return commit()
	}()
	if outcome == nil {
		return nil
	}
	var restoration []error
	for i := written - 1; i >= 0; i-- {
		p := paths[i]
		now, err := readPrivateMaterial(p)
		if err != nil || !bytes.Equal(now, contents[i]) {
			restoration = append(restoration, invalid("回滚目标发生变化："+DisplayPath(p)))
			continue
		}
		if exists[i] {
			err = writePrivateAtomic(p, before[i])
		} else {
			err = os.Remove(p)
		}
		if err != nil {
			restoration = append(restoration, err)
		}
	}
	if len(restoration) > 0 {
		return &AppError{Code: "DEPLOY_UNCERTAIN", Message: "证书文件未能完整恢复，请核对备份后手动重试", Hint: DisplayPath(backup), Detail: errors.Join(restoration...).Error()}
	}
	return outcome
}
func (c *Core) deployLocalTarget(ctx context.Context, target certDeployTarget, m certIssuedMaterial) (string, error) {
	if err := c.deployCertFiles(ctx, []string{target.Config["certPath"], target.Config["keyPath"]}, [][]byte{[]byte(m.Chain), []byte(m.KeyPEM)}, func() error { return nil }); err != nil {
		return "", err
	}
	if script := target.Config["script"]; strings.TrimSpace(script) != "" {
		timeout, _ := certDeployTimeout(target)
		runCtx, cancel := context.WithTimeout(ctx, timeout)
		defer cancel()
		out, stderr := process.NewLimitedBuffer(32768), process.NewLimitedBuffer(32768)
		spec, err := process.Shell(script, c.Home, out, stderr)
		if err != nil {
			return "", err
		}
		child, err := process.Start(runCtx, spec)
		if err != nil {
			return "", err
		}
		result := child.Wait()
		if result.Err != nil || result.Code != 0 {
			return "", &AppError{Code: "DEPLOY_UNCERTAIN", Message: "证书已写入，但部署脚本未成功完成，请核对服务后手动重试"}
		}
	}
	return "本地证书和私钥已部署", nil
}

func (c *Core) deployLocalSites(ctx context.Context, a certAutomation, m certIssuedMaterial) (string, string, error) {
	unlock, err := c.certLock()
	if err != nil {
		return "", "", err
	}
	defer unlock()
	chain, _, err := certPair([]byte(m.Chain), []byte(m.KeyPEM))
	if err != nil {
		return "", "", err
	}
	leaf := chain[0]
	sites, err := c.certificateSites(ctx)
	if err != nil {
		return "", "", err
	}
	subjects := []string{a.Domains[0]}
	selected := map[string]bool{a.Domains[0]: true}
	for _, site := range sites {
		if !site.HTTPS || site.Runtime.ImportedCertID != "" || site.Runtime.ACMECertID != "" || len(site.Domains) == 0 {
			continue
		}
		subject := site.Domains[0]
		covered := true
		for _, d := range site.Domains {
			covered = covered && certCovers(leaf, d)
		}
		if !covered {
			continue
		}
		protected := false
		for _, other := range sites {
			if subject != a.Domains[0] && other.Runtime.ACMECertID == "acme-"+subject {
				protected = true
			}
		}
		if !protected && !selected[subject] {
			selected[subject] = true
			subjects = append(subjects, subject)
		}
	}
	for _, site := range sites {
		subject := ""
		if site.Runtime.ImportedCertID != "" {
			continue
		}
		if site.Runtime.ACMECertID != "" {
			subject = strings.TrimPrefix(site.Runtime.ACMECertID, "acme-")
		} else if len(site.Domains) > 0 {
			subject = site.Domains[0]
		}
		if selected[subject] {
			for _, d := range site.Domains {
				if !certCovers(leaf, d) {
					return "", "", invalid("新证书未覆盖使用同一输出文件的站点域名：" + d)
				}
			}
		}
	}
	var paths []string
	var contents [][]byte
	var records []certRecord
	for _, subject := range subjects {
		cert := filepath.Join(c.Home, "certs", "sites", certStem(subject)+".crt")
		key := filepath.Join(c.Home, "certs", "sites", certStem(subject)+".key")
		if err = managedDirectory(c.Home, filepath.Dir(cert)); err != nil {
			return "", "", err
		}
		paths = append(paths, cert, key)
		contents = append(contents, []byte(m.Chain), []byte(m.KeyPEM))
		records = append(records, certRecord{ID: "acme-" + subject, Kind: "acme", Subject: subject, SANs: certSANs(leaf), NotBefore: leaf.NotBefore.UnixMilli(), NotAfter: leaf.NotAfter.UnixMilli(), CertPath: Path(cert), KeyPath: Path(key)})
	}
	err = c.deployCertFiles(ctx, paths, contents, func() error {
		tx, err := c.Store.db.BeginTx(ctx, nil)
		if err != nil {
			return err
		}
		defer tx.Rollback()
		for _, r := range records {
			sans, _ := json.Marshal(r.SANs)
			if _, err = tx.ExecContext(ctx, "DELETE FROM certs WHERE cert_path=? AND id<>?", string(r.CertPath), r.ID); err != nil {
				return err
			}
			if _, err = tx.ExecContext(ctx, `INSERT INTO certs(id,kind,subject,sans,not_before,not_after,cert_path,key_path) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,subject=excluded.subject,sans=excluded.sans,not_before=excluded.not_before,not_after=excluded.not_after,cert_path=excluded.cert_path,key_path=excluded.key_path`, r.ID, r.Kind, r.Subject, string(sans), r.NotBefore, r.NotAfter, string(r.CertPath), string(r.KeyPath)); err != nil {
				return err
			}
		}
		return tx.Commit()
	})
	if err != nil {
		return "", "", err
	}
	for _, server := range []string{"nginx", "apache", "caddy"} {
		if err = c.reloadWebIfRunning(ctx, server); err != nil {
			return "", "", &AppError{Code: "CERT_DEPLOY_FAILED", Message: "证书已保存，但 Web 服务重载失败", Detail: err.Error()}
		}
	}
	return records[0].ID, fmt.Sprintf("已更新 %d 组本地证书文件并重载运行中的 Web 服务", len(records)), nil
}

func certDeployTimeout(target certDeployTarget) (time.Duration, error) {
	seconds := 120
	if raw := target.Config["timeoutSec"]; raw != "" {
		var err error
		seconds, err = strconv.Atoi(raw)
		if err != nil {
			return 0, invalid("部署脚本超时应为整数秒")
		}
	}
	if seconds < 1 || seconds > 3600 {
		return 0, invalid("部署超时须在 1–3600 秒内")
	}
	return time.Duration(seconds) * time.Second, nil
}
