package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"net"
	"net/mail"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"time"
)

type certDNS struct {
	Kind      string `json:"kind"`
	AccessKey string `json:"accessKey"`
	Secret    string `json:"secret"`
}
type certDeployResult struct {
	OK      bool   `json:"ok"`
	Message string `json:"message"`
	At      int64  `json:"at"`
}
type certDeployTarget struct {
	ID         string            `json:"id"`
	Kind       string            `json:"kind"`
	Name       string            `json:"name"`
	Config     map[string]string `json:"config"`
	LastResult *certDeployResult `json:"lastResult,omitempty"`
}
type certRun struct {
	At      int64    `json:"at"`
	OK      bool     `json:"ok"`
	Message string   `json:"message"`
	Log     []string `json:"log"`
}
type certTXT struct {
	Name  string `json:"name"`
	Value string `json:"value"`
}
type certSMTP struct {
	Host        string `json:"host"`
	Port        int    `json:"port"`
	Username    string `json:"username"`
	Password    string `json:"password"`
	From        string `json:"from"`
	To          string `json:"to"`
	ImplicitTLS bool   `json:"implicitTls"`
}
type certAutomation struct {
	ID                string             `json:"id"`
	Name              string             `json:"name"`
	Domains           []string           `json:"domains"`
	Email             string             `json:"email"`
	CA                string             `json:"ca"`
	DNS               certDNS            `json:"dns"`
	DeployLocal       bool               `json:"deployLocal"`
	DeploymentID      string             `json:"deploymentId"`
	LocalDeployResult *certDeployResult  `json:"localDeployResult"`
	Targets           []certDeployTarget `json:"targets"`
	Enabled           bool               `json:"enabled"`
	State             string             `json:"state"`
	LastError         string             `json:"lastError"`
	CertID            *string            `json:"certId,omitempty"`
	IssuedAt          *int64             `json:"issuedAt,omitempty"`
	ExpiresAt         *int64             `json:"expiresAt,omitempty"`
	NextRenewAt       int64              `json:"nextRenewAt"`
	LastRunAt         int64              `json:"lastRunAt"`
	KeyAlg            string             `json:"keyAlg"`
	EABKID            string             `json:"eabKid"`
	EABHMACKey        string             `json:"eabHmacKey"`
	DNSWaitSec        int                `json:"dnsWaitSec"`
	CNAMETarget       string             `json:"cnameTarget"`
	RenewDaysAhead    int                `json:"renewDaysAhead"`
	RetryTimes        int                `json:"retryTimes"`
	RetryIntervalMin  int                `json:"retryIntervalMin"`
	FailCount         int                `json:"failCount"`
	NotifyKind        string             `json:"notifyKind"`
	NotifySMTP        *certSMTP          `json:"notifySmtp,omitempty"`
	NotifyURL         string             `json:"notifyUrl"`
	Runs              []certRun          `json:"runs"`
	ManualRecords     []certTXT          `json:"manualRecords"`
	CreatedAt         int64              `json:"createdAt"`
	UpdatedAt         int64              `json:"updatedAt"`
}
type certAutomationManager struct{ mu sync.Mutex }

func certAutoBusy() error {
	return &AppError{Code: "CERT_AUTO_BUSY", Message: "证书自动化正在执行或修改，请等待当前操作完成"}
}
func certAutoID(id string) bool { return siteIdentifier.MatchString(id) && len(id) <= 128 }
func (c *Core) certAutomation(ctx context.Context, id string) (certAutomation, error) {
	var a certAutomation
	if !certAutoID(id) {
		return a, invalid("证书自动化标识无效")
	}
	var raw string
	err := c.Store.db.QueryRowContext(ctx, "SELECT data FROM cert_automations WHERE id=?", id).Scan(&raw)
	if errors.Is(err, sql.ErrNoRows) {
		return a, &AppError{Code: "NOT_FOUND", Message: "证书自动化已不存在"}
	}
	if err != nil {
		return a, err
	}
	if err = json.Unmarshal([]byte(raw), &a); err != nil {
		return a, invalid("证书自动化记录损坏")
	}
	if a.ID != id {
		return a, invalid("证书自动化记录身份不一致")
	}
	return a, nil
}
func (c *Core) saveCertAutomation(ctx context.Context, a *certAutomation) error {
	a.UpdatedAt = max(time.Now().UnixMilli(), a.UpdatedAt+1)
	if a.Runs == nil {
		a.Runs = []certRun{}
	}
	if a.Targets == nil {
		a.Targets = []certDeployTarget{}
	}
	if a.ManualRecords == nil {
		a.ManualRecords = []certTXT{}
	}
	raw, err := json.Marshal(a)
	if err != nil {
		return err
	}
	_, err = c.Store.db.ExecContext(ctx, "INSERT INTO cert_automations(id,data,updated_at) VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at", a.ID, string(raw), a.UpdatedAt)
	if err == nil && c.emit != nil {
		c.emit("certauto://progress", map[string]any{"id": a.ID, "state": a.State, "message": a.LastError})
	}
	return err
}
func validateCertAutomation(a *certAutomation) error {
	if !certAutoID(a.ID) {
		return invalid("证书自动化标识无效")
	}
	domains, err := normalizeCertDomains(a.Domains)
	if err != nil {
		return err
	}
	a.Domains = domains
	for _, domain := range domains {
		if net.ParseIP(domain) != nil || !strings.Contains(domain, ".") || domain == "localhost" {
			return invalid("自动签发须使用完整域名")
		}
	}
	if a.Name == "" {
		a.Name = domains[0]
	}
	if len(a.Name) > 256 {
		return invalid("自动化名称过长")
	}
	if !slices.Contains([]string{"letsencrypt", "letsencrypt-staging", "zerossl", "google", "buypass"}, a.CA) {
		return invalid("证书颁发机构无效")
	}
	if slices.Contains([]string{"zerossl", "google", "buypass"}, a.CA) && (a.EABKID == "" || a.EABHMACKey == "") {
		return invalid("此颁发机构需要 EAB KID 和 HMAC Key")
	}
	if a.Email != "" {
		address, err := mail.ParseAddress(a.Email)
		if err != nil || address.Address != a.Email {
			return invalid("联系邮箱格式无效")
		}
	}
	if !slices.Contains([]string{"ec256", "ec384", "rsa2048", "rsa3072", "rsa4096"}, a.KeyAlg) {
		return invalid("证书私钥算法无效")
	}
	if a.RenewDaysAhead < 1 || a.RenewDaysAhead > 90 || a.RetryTimes < 0 || a.RetryTimes > 100 || a.RetryIntervalMin < 1 || a.RetryIntervalMin > 1440 || a.DNSWaitSec < 0 || a.DNSWaitSec > 3600 {
		return invalid("续签或 DNS 等待时间超出允许范围")
	}
	if !slices.Contains([]string{"manual", "aliyun", "cloudflare", "dnspod", "huawei", "godaddy", "digitalocean", "porkbun"}, a.DNS.Kind) {
		return invalid("DNS 服务商无效")
	}
	if a.DNS.Kind != "manual" && (strings.TrimSpace(a.DNS.AccessKey) == "" || (!slices.Contains([]string{"cloudflare", "digitalocean"}, a.DNS.Kind) && strings.TrimSpace(a.DNS.Secret) == "")) {
		return invalid("DNS 服务商凭据不完整")
	}
	if a.CNAMETarget != "" {
		for _, domain := range domains {
			target := strings.ReplaceAll(a.CNAMETarget, "{domain}", strings.TrimPrefix(domain, "*."))
			if _, err := normalizeCertDomains([]string{target}); err != nil || strings.Contains(target, "*") {
				return invalid("CNAME 代理验证域名无效")
			}
		}
	}
	ids := map[string]bool{}
	if len(a.Targets) > 100 {
		return invalid("最多保存 100 个部署目标")
	}
	for _, target := range a.Targets {
		if target.ID == "" || ids[target.ID] {
			return invalid("部署目标标识为空或重复")
		}
		ids[target.ID] = true
		if err := validateCertDeployTarget(target); err != nil {
			return err
		}
	}
	if a.NotifyKind == "" {
		a.NotifyKind = "none"
	}
	if a.NotifyKind == "email" {
		if err := validateCertSMTP(a.NotifySMTP); err != nil {
			return err
		}
	} else if err := validateMonitorNotification(monitorNotification{Kind: a.NotifyKind, URL: a.NotifyURL}); err != nil {
		return err
	}
	return nil
}
func (c *Core) recoverCertAutomations(ctx context.Context) error {
	rows, err := c.Store.tableRows(ctx, "SELECT id,data FROM cert_automations", nil, nil)
	if err != nil {
		return err
	}
	for _, row := range rows {
		var a certAutomation
		if json.Unmarshal([]byte(row["data"].(string)), &a) != nil || !certAutoID(a.ID) || a.ID != row["id"] {
			return invalid("证书自动化记录无效")
		}
		if slices.Contains([]string{"issuing", "manual_wait", "deploying"}, a.State) {
			a.Enabled = false
			a.LastError = "上次证书任务已中断，已暂停自动续签。请检查 DNS 记录和部署目标后手动重试。"
			if a.DeploymentID != "" {
				a.State = "deploy_interrupted"
			} else {
				a.State = "error"
			}
			a.NextRenewAt = 0
			a.Runs = append([]certRun{{At: time.Now().UnixMilli(), Message: a.LastError, Log: []string{a.LastError}}}, a.Runs...)
			a.Runs = a.Runs[:min(20, len(a.Runs))]
			if err = c.saveCertAutomation(ctx, &a); err != nil {
				return err
			}
		}
	}
	return nil
}
func (c *Core) registerCertAutomations() {
	c.handle("certauto_list", func(ctx context.Context, _ json.RawMessage) (any, error) {
		rows, err := c.Store.tableRows(ctx, "SELECT id,data FROM cert_automations ORDER BY updated_at DESC,id", nil, nil)
		if err != nil {
			return nil, err
		}
		out := []certAutomation{}
		for _, row := range rows {
			var a certAutomation
			if json.Unmarshal([]byte(row["data"].(string)), &a) != nil {
				return nil, invalid("证书自动化记录损坏")
			}
			out = append(out, a)
		}
		return out, nil
	})
	c.handle("certauto_save", c.saveCertAuto)
	actionHandler := func(action string) Handler {
		return func(ctx context.Context, raw json.RawMessage) (any, error) {
			in, err := decode[struct {
				ID      string `json:"id"`
				Enabled bool   `json:"enabled"`
			}](raw)
			if err != nil {
				return nil, err
			}
			if !c.certAuto.mu.TryLock() {
				return nil, certAutoBusy()
			}
			defer c.certAuto.mu.Unlock()
			a, err := c.certAutomation(ctx, in.ID)
			if err != nil {
				return nil, err
			}
			switch action {
			case "issue", "retry_deploy":
				return c.runCertAutomation(ctx, a, action == "retry_deploy")
			case "set_enabled":
				if in.Enabled {
					if err = validateCertAutomation(&a); err != nil {
						return nil, err
					}
					if a.State == "deploy_interrupted" {
						return nil, invalid("请先核对目标端并手动重试部署")
					}
				}
				a.Enabled = in.Enabled
				a.NextRenewAt = 0
				if a.Enabled {
					a.NextRenewAt = time.Now().UnixMilli()
					if a.State == "ok" && a.ExpiresAt != nil {
						a.NextRenewAt = max(a.NextRenewAt, *a.ExpiresAt-int64(a.RenewDaysAhead)*86400000)
					}
				}
				if err = c.saveCertAutomation(ctx, &a); err != nil {
					return nil, err
				}
				return a, nil
			case "delete":
				dir := filepath.Join(c.Home, "certs", "acme")
				if err = managedDirectory(c.Home, dir); err != nil {
					return nil, err
				}
				saved := map[string][]byte{}
				for _, suffix := range []string{".account.key", ".issued.json"} {
					path := filepath.Join(dir, a.ID+suffix)
					if info, err := os.Lstat(path); os.IsNotExist(err) {
						continue
					} else if err != nil {
						return nil, err
					} else if !info.Mode().IsRegular() || info.Size() > 8*1024*1024 {
						return nil, invalid("签发材料不是有效普通文件")
					}
					body, err := os.ReadFile(path)
					if err != nil {
						return nil, err
					}
					saved[path] = body
				}
				var removed []string
				rollback := func(err error) error {
					for _, path := range removed {
						err = errors.Join(err, writePrivateAtomic(path, saved[path]))
					}
					return err
				}
				for path := range saved {
					if err = os.Remove(path); err != nil {
						return nil, rollback(err)
					}
					removed = append(removed, path)
				}
				_, err = c.Store.db.ExecContext(ctx, "DELETE FROM cert_automations WHERE id=?", a.ID)
				if err != nil {
					return nil, rollback(err)
				}
				return true, nil
			}
			return nil, invalid("证书操作无效")
		}
	}
	c.handle("certauto_delete", actionHandler("delete"))
	c.handle("certauto_set_enabled", actionHandler("set_enabled"))
	c.handle("certauto_issue", actionHandler("issue"))
	c.handle("certauto_retry_deploy", actionHandler("retry_deploy"))
}
func (c *Core) saveCertAuto(ctx context.Context, raw json.RawMessage) (any, error) {
	in, err := decode[struct {
		A certAutomation `json:"a"`
	}](raw)
	if err != nil {
		return nil, err
	}
	a := in.A
	creating := a.ID == ""
	if creating {
		a.ID, err = newID("auto")
		if err != nil {
			return nil, err
		}
	}
	if err = validateCertAutomation(&a); err != nil {
		return nil, err
	}
	if !c.certAuto.mu.TryLock() {
		return nil, certAutoBusy()
	}
	defer c.certAuto.mu.Unlock()
	now := time.Now().UnixMilli()
	if creating {
		a.State = "idle"
		a.LastError = ""
		a.DeploymentID = ""
		a.LocalDeployResult = nil
		a.CertID = nil
		a.IssuedAt = nil
		a.ExpiresAt = nil
		a.LastRunAt = 0
		a.FailCount = 0
		a.Runs = []certRun{}
		a.ManualRecords = []certTXT{}
		a.CreatedAt = now
		a.UpdatedAt = now
		a.NextRenewAt = 0
		if a.Enabled {
			a.NextRenewAt = now
		}
		for i := range a.Targets {
			a.Targets[i].LastResult = nil
		}
	} else {
		old, err := c.certAutomation(ctx, a.ID)
		if err != nil {
			return nil, err
		}
		if a.UpdatedAt != old.UpdatedAt {
			return nil, &AppError{Code: "CERT_AUTO_CONFLICT", Message: "自动化记录已更新，请重新打开编辑"}
		}
		identity := !reflect.DeepEqual(a.Domains, old.Domains) || a.CA != old.CA || a.KeyAlg != old.KeyAlg
		a.State, a.LastError, a.CertID, a.IssuedAt, a.ExpiresAt, a.LastRunAt, a.FailCount, a.Runs, a.ManualRecords, a.Enabled, a.CreatedAt, a.NextRenewAt, a.DeploymentID = old.State, old.LastError, old.CertID, old.IssuedAt, old.ExpiresAt, old.LastRunAt, old.FailCount, old.Runs, old.ManualRecords, old.Enabled, old.CreatedAt, old.NextRenewAt, old.DeploymentID
		a.LocalDeployResult = old.LocalDeployResult
		if a.DeployLocal != old.DeployLocal {
			a.LocalDeployResult = nil
			a.CertID = nil
		}
		for i := range a.Targets {
			a.Targets[i].LastResult = nil
			for _, target := range old.Targets {
				if target.ID == a.Targets[i].ID && target.Kind == a.Targets[i].Kind && reflect.DeepEqual(target.Config, a.Targets[i].Config) {
					a.Targets[i].LastResult = target.LastResult
				}
			}
		}
		if identity {
			a.State = "idle"
			a.LastError = ""
			a.CertID = nil
			a.IssuedAt = nil
			a.ExpiresAt = nil
			a.FailCount = 0
			a.DeploymentID = ""
			a.LocalDeployResult = nil
			a.NextRenewAt = 0
			if a.Enabled {
				a.NextRenewAt = now
			}
			for i := range a.Targets {
				a.Targets[i].LastResult = nil
			}
		} else if a.DeploymentID != "" && a.State != "deploy_interrupted" {
			pending := a.DeployLocal && a.LocalDeployResult == nil
			for _, target := range a.Targets {
				pending = pending || target.LastResult == nil
			}
			if pending {
				a.State = "deploy_error"
				a.LastError = "部署配置已更新，请重试部署以应用已签发证书"
				if a.Enabled {
					a.NextRenewAt = now
				}
			} else if a.State == "ok" && a.Enabled && a.ExpiresAt != nil {
				a.NextRenewAt = *a.ExpiresAt - int64(a.RenewDaysAhead)*86400000
			}
		}
	}
	if err = c.saveCertAutomation(ctx, &a); err != nil {
		return nil, err
	}
	return a, nil
}
