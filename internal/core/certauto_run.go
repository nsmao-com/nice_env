package core

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"path/filepath"
	"reflect"
	"strings"
	"time"
)

func certAutoError(a certAutomation, err error) string {
	if err == nil {
		return ""
	}
	message := err.Error()
	secrets := []string{a.DNS.AccessKey, a.DNS.Secret, a.EABKID, a.EABHMACKey, a.NotifyURL}
	if a.NotifySMTP != nil {
		secrets = append(secrets, a.NotifySMTP.Password)
	}
	for _, target := range a.Targets {
		for _, k := range []string{"password", "keyPassphrase", "apiSk", "token", "accessKeySecret", "secretKey"} {
			secrets = append(secrets, target.Config[k])
		}
	}
	for _, secret := range secrets {
		if secret != "" {
			message = strings.ReplaceAll(message, secret, "[已隐藏]")
			message = strings.ReplaceAll(message, url.QueryEscape(secret), "[已隐藏]")
		}
	}
	runes := []rune(message)
	if len(runes) > 2000 {
		message = string(runes[:2000]) + "…"
	}
	return message
}
func (c *Core) readIssuedCert(a certAutomation) (certIssuedMaterial, error) {
	var m certIssuedMaterial
	if err := managedDirectory(c.Home, filepath.Join(c.Home, "certs", "acme")); err != nil {
		return m, err
	}
	data, err := readPrivateMaterial(filepath.Join(c.Home, "certs", "acme", a.ID+".issued.json"))
	if err != nil {
		return m, &AppError{Code: "CERT_DEPLOY_MISSING", Message: "已签发证书材料无法读取，请检查备份"}
	}
	if err = json.Unmarshal(data, &m); err != nil {
		return m, invalid("已签发证书材料损坏")
	}
	return m, validateIssuedCert(a, m, true)
}
func validateIssuedCert(a certAutomation, m certIssuedMaterial, retry bool) error {
	if m.Version != 1 || m.AutomationID != a.ID || m.DeploymentID == "" || retry && m.DeploymentID != a.DeploymentID || m.CA != a.CA || m.KeyAlg != a.KeyAlg || !reflect.DeepEqual(m.Domains, a.Domains) {
		return invalid("已签发证书材料与自动化配置不一致")
	}
	chain, _, err := certPair([]byte(m.Chain), []byte(m.KeyPEM))
	if err != nil {
		return err
	}
	if err = certValidity(chain[0], true); err != nil {
		return err
	}
	for _, domain := range a.Domains {
		if !certCovers(chain[0], domain) {
			return invalid("签发证书未覆盖请求域名：" + domain)
		}
	}
	return nil
}
func (c *Core) retainIssuedCert(ctx context.Context, a *certAutomation, m certIssuedMaterial) error {
	if err := validateIssuedCert(*a, m, false); err != nil {
		return err
	}
	dir := filepath.Join(c.Home, "certs", "acme")
	if err := managedDirectory(c.Home, dir); err != nil {
		return err
	}
	data, err := json.Marshal(m)
	if err != nil {
		return err
	}
	if err = writePrivateAtomic(filepath.Join(dir, a.ID+".issued.json"), data); err != nil {
		return err
	}
	chain, _, _ := certPair([]byte(m.Chain), []byte(m.KeyPEM))
	now, expiry := time.Now().UnixMilli(), chain[0].NotAfter.UnixMilli()
	a.DeploymentID = m.DeploymentID
	a.CertID = nil
	a.IssuedAt = &now
	a.ExpiresAt = &expiry
	a.LocalDeployResult = nil
	for i := range a.Targets {
		a.Targets[i].LastResult = nil
	}
	a.State = "deploying"
	return c.saveCertAutomation(ctx, a)
}

// Caller holds certAuto.mu. Network work never holds the service/store locks.
// Persist each target acknowledgement before touching the next target so a
// retry cannot replay a target whose successful result is already durable.
func (c *Core) runCertAutomation(ctx context.Context, a certAutomation, retry bool) (certAutomation, error) {
	if err := validateCertAutomation(&a); err != nil {
		return a, err
	}
	if retry && a.DeploymentID == "" {
		return a, invalid("没有可复用的签发批次")
	}
	ctx, cancel := context.WithTimeout(ctx, 90*time.Minute)
	defer cancel()
	go func() {
		select {
		case <-c.closed:
			cancel()
		case <-ctx.Done():
		}
	}()
	a.LastError = ""
	a.LastRunAt = time.Now().UnixMilli()
	a.State = "issuing"
	if retry {
		a.State = "deploying"
	} else {
		a.ManualRecords = []certTXT{}
	}
	if err := c.saveCertAutomation(ctx, &a); err != nil {
		return a, err
	}
	log := []string{"开始处理证书：" + strings.Join(a.Domains, ", ")}
	uncertain := false
	outcome := func() error {
		var m certIssuedMaterial
		var err error
		if retry {
			m, err = c.readIssuedCert(a)
			log = append(log, "复用已签发证书，只重试未完成部署")
			if err != nil {
				uncertain = true
				return err
			}
		} else {
			m, err = c.issueACMEMaterial(ctx, &a)
			if m.DeploymentID != "" {
				saveCtx, done := context.WithTimeout(context.Background(), 10*time.Second)
				saveErr := c.retainIssuedCert(saveCtx, &a, m)
				done()
				if saveErr != nil {
					uncertain = true
					return errors.Join(err, saveErr)
				}
				log = append(log, "证书签发成功，已保存证书和私钥")
			}
			if err != nil {
				if m.DeploymentID != "" || len(a.ManualRecords) > 0 && a.DNS.Kind != "manual" {
					uncertain = true
				}
				return err
			}
		}
		var failures []error
		if a.DeployLocal && (a.LocalDeployResult == nil || !a.LocalDeployResult.OK) {
			id, message, err := c.deployLocalSites(ctx, a, m)
			a.LocalDeployResult = &certDeployResult{OK: err == nil, Message: message, At: time.Now().UnixMilli()}
			if err != nil {
				a.LocalDeployResult.Message = certAutoError(a, err)
				failures = append(failures, err)
				var app *AppError
				if errors.As(err, &app) && app.Code == "DEPLOY_UNCERTAIN" {
					uncertain = true
				}
			} else {
				a.CertID = &id
			}
			log = append(log, a.LocalDeployResult.Message)
			if err = c.saveCertAutomation(ctx, &a); err != nil {
				uncertain = true
				return err
			}
		}
		for i := range a.Targets {
			if uncertain {
				break
			}
			target := a.Targets[i]
			if target.LastResult != nil && target.LastResult.OK {
				continue
			}
			if err = ctx.Err(); err != nil {
				uncertain = true
				return err
			}
			message, deployErr := c.deployCertTarget(ctx, target, m)
			a.Targets[i].LastResult = &certDeployResult{OK: deployErr == nil, Message: message, At: time.Now().UnixMilli()}
			if deployErr != nil {
				a.Targets[i].LastResult.Message = certAutoError(a, deployErr)
				failures = append(failures, deployErr)
				var app *AppError
				if errors.As(deployErr, &app) && app.Code == "DEPLOY_UNCERTAIN" {
					uncertain = true
				}
			}
			log = append(log, target.Name+"："+a.Targets[i].LastResult.Message)
			if err = c.saveCertAutomation(ctx, &a); err != nil {
				uncertain = true
				return err
			}
			if uncertain {
				break
			}
		}
		return errors.Join(failures...)
	}()
	now := time.Now().UnixMilli()
	message := "签发与部署完成"
	if outcome == nil {
		a.State = "ok"
		a.FailCount = 0
		a.LastError = ""
		a.NextRenewAt = now + 60*86400000
		if a.ExpiresAt != nil {
			a.NextRenewAt = max(now+3600000, *a.ExpiresAt-int64(a.RenewDaysAhead)*86400000)
		}
	} else {
		a.LastError = certAutoError(a, outcome)
		message = a.LastError
		a.FailCount++
		a.State = "error"
		if a.DeploymentID != "" && a.IssuedAt != nil && (*a.IssuedAt >= a.LastRunAt || retry) {
			a.State = "deploy_error"
		}
		a.NextRenewAt = now + int64(a.RetryIntervalMin)*60000
		if a.FailCount > max(1, a.RetryTimes) {
			a.NextRenewAt = now + 86400000
		}
		if uncertain || ctx.Err() != nil {
			a.Enabled = false
			a.NextRenewAt = 0
			if a.State == "deploy_error" {
				a.State = "deploy_interrupted"
			}
			a.LastError += "；自动执行已暂停，请检查后手动重试"
		}
	}
	log = append(log, a.LastError)
	a.Runs = append([]certRun{{At: now, OK: outcome == nil, Message: message, Log: log}}, a.Runs...)
	a.Runs = a.Runs[:min(20, len(a.Runs))]
	saveCtx, saveCancel := context.WithTimeout(context.Background(), 10*time.Second)
	err := c.saveCertAutomation(saveCtx, &a)
	saveCancel()
	if err != nil {
		return a, err
	}
	if ctx.Err() == nil {
		title := "NiceEnv 证书自动化：" + strings.Join(a.Domains, ", ")
		var notifyErr error
		if a.NotifyKind == "email" {
			notifyErr = sendCertEmail(ctx, a.NotifySMTP, title, message)
		} else {
			notifyErr = sendCertWebhook(ctx, monitorNotification{Kind: a.NotifyKind, URL: a.NotifyURL}, strings.Join(a.Domains, ", "), a.State, message)
		}
		if notifyErr != nil {
			a.Runs[0].Log = append(a.Runs[0].Log, "通知发送失败："+certAutoError(a, notifyErr))
			if err = c.saveCertAutomation(ctx, &a); err != nil {
				return a, err
			}
		}
	}
	return a, nil
}

func (c *Core) certAutoScheduler(ctx context.Context) {
	defer c.cron.wg.Done()
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	go func() {
		select {
		case <-c.closed:
			cancel()
		case <-ctx.Done():
		}
	}()
	timer := time.NewTimer(30 * time.Second)
	defer timer.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-timer.C:
		}
		if c.beginDataActivity() {
			if c.certAuto.mu.TryLock() {
				c.certAutoTick(ctx)
				c.certAuto.mu.Unlock()
			}
			c.dataDir.activity.RUnlock()
		}
		timer.Reset(time.Minute)
	}
}
func (c *Core) certAutoTick(ctx context.Context) {
	rows, err := c.Store.tableRows(ctx, "SELECT id,data FROM cert_automations ORDER BY updated_at", nil, nil)
	if err != nil {
		return
	}
	for _, row := range rows {
		if ctx.Err() != nil {
			return
		}
		var a certAutomation
		if json.Unmarshal([]byte(row["data"].(string)), &a) != nil || a.ID != row["id"] {
			continue
		}
		if !a.Enabled || a.NextRenewAt > time.Now().UnixMilli() || a.State == "deploy_interrupted" {
			continue
		}
		retry := a.State == "deploy_error" || a.State == "deploy_waiting"
		if a.DNS.Kind == "manual" && !retry {
			a.LastError = "证书需要手动 DNS 验证，请点击立即签发添加 TXT 记录"
			a.NextRenewAt = time.Now().Add(24 * time.Hour).UnixMilli()
			_ = c.saveCertAutomation(ctx, &a)
			continue
		}
		if _, err = c.runCertAutomation(ctx, a, retry); err != nil {
			a.Enabled = false
			a.NextRenewAt = 0
			a.LastError = certAutoError(a, err)
			_ = c.saveCertAutomation(ctx, &a)
		}
	}
}
