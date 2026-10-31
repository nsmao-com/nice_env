package core

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	"niceenv/internal/hostfile"
	"niceenv/internal/process"
)

type healthItem struct {
	ID       string `json:"id"`
	Severity string `json:"severity"`
	Title    string `json:"title"`
	Detail   string `json:"detail"`
	Action   string `json:"action,omitempty"`
	Route    string `json:"route,omitempty"`
}
type healthCoverage struct {
	ID     string `json:"id"`
	Label  string `json:"label"`
	State  string `json:"state"`
	Detail string `json:"detail"`
}
type healthReport struct {
	Items     []healthItem     `json:"items"`
	Checks    []healthCoverage `json:"checks"`
	Errors    int              `json:"errors"`
	Warnings  int              `json:"warnings"`
	Infos     int              `json:"infos"`
	Summary   string           `json:"summary"`
	CheckedAt int64            `json:"checkedAt"`
}

func (r *healthReport) issue(id, severity, title, detail, route string) {
	r.Items = append(r.Items, healthItem{ID: id, Severity: severity, Title: title, Detail: redactDiagnostic(detail), Action: "检查相关配置后重新体检", Route: route})
	switch severity {
	case "error":
		r.Errors++
	case "warn":
		r.Warnings++
	default:
		r.Infos++
	}
}
func (r *healthReport) coverage(id, label, state, detail string) {
	r.Checks = append(r.Checks, healthCoverage{ID: id, Label: label, State: state, Detail: redactDiagnostic(detail)})
}
func (r *healthReport) unavailable(id, label, route string, err error) {
	r.coverage(id, label, "unavailable", err.Error())
	r.issue(id+"-unavailable", "warn", "未完成"+label+"检查", err.Error(), route)
}
func (r *healthReport) finish(empty bool) {
	sort.SliceStable(r.Items, func(i, j int) bool {
		return map[string]int{"error": 0, "warn": 1, "info": 2}[r.Items[i].Severity] < map[string]int{"error": 0, "warn": 1, "info": 2}[r.Items[j].Severity]
	})
	incomplete := 0
	for _, check := range r.Checks {
		if check.State == "unavailable" {
			incomplete++
		}
	}
	switch {
	case r.Errors > 0:
		r.Summary = fmt.Sprintf("发现 %d 个需要处理的问题", r.Errors)
	case incomplete > 0:
		r.Summary = fmt.Sprintf("%d 项检查未完成，请查看原因", incomplete)
	case r.Warnings > 0:
		r.Summary = fmt.Sprintf("%d 项建议处理", r.Warnings)
	case empty:
		r.Summary = "尚未配置环境"
	default:
		r.Summary = "已检查范围内未发现问题"
	}
	r.CheckedAt = time.Now().Unix()
}
func (c *Core) healthCheck(ctx context.Context) (healthReport, error) {
	r := healthReport{Items: []healthItem{}, Checks: []healthCoverage{}}
	if !c.services.lifecycle.TryLock() {
		return r, &AppError{Code: "HEALTH_BUSY", Message: "正在体检或调整服务，请稍后重新检查"}
	}
	defer c.services.lifecycle.Unlock()
	if !c.siteChanges.TryLock() {
		return r, serviceBusy()
	}
	defer c.siteChanges.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 45*time.Second)
	defer cancel()
	installed, err := c.Store.tableRows(ctx, "SELECT id,version,install_path AS installPath FROM installed ORDER BY id,version", nil, nil)
	if err != nil {
		return r, err
	}
	has := map[string]bool{}
	for _, p := range installed {
		id, version, path := p["id"].(string), p["version"].(string), p["installPath"].(string)
		has[id] = true
		has[id+"@"+canonicalVersion(version)] = true
		if info, err := os.Stat(path); err != nil || !info.IsDir() {
			r.issue("install-"+id+"-"+version, "error", id+" "+version+" 安装目录缺失或无法访问", DisplayPath(path), "/packages")
		}
	}
	if len(installed) == 0 {
		r.issue("no-packages", "info", "还没有安装套件", "先安装 Web 服务器与运行时，再创建站点", "/packages")
	}
	r.coverage("packages", "安装记录与目录", "checked", fmt.Sprintf("%d 个安装记录；检查目录存在性，不验证全部文件完整性", len(installed)))
	c.healthPorts(ctx, &r)
	sites, err := c.sites(ctx)
	if err != nil {
		r.unavailable("sites", "站点依赖与路径", "/sites", err)
	} else {
		for _, site := range sites {
			problems := []string{}
			if info, err := os.Stat(string(site.RootDir)); err != nil || !info.IsDir() || !filepath.IsAbs(string(site.RootDir)) {
				problems = append(problems, "根目录不存在或无法访问："+DisplayPath(string(site.RootDir)))
			}
			web := site.Runtime.WebServer
			if web != "nginx" && web != "apache" && web != "caddy" {
				problems = append(problems, "Web 服务器类型无效")
			} else if !has[web] {
				problems = append(problems, "未安装 "+web)
			}
			if len(site.Domains) == 0 {
				problems = append(problems, "未配置域名")
			}
			if site.Runtime.Kind == "php" {
				if site.Runtime.PHPVersion == nil || !has["php@"+canonicalVersion(*site.Runtime.PHPVersion)] {
					problems = append(problems, "未安装指定 PHP 版本")
				}
			} else if site.Runtime.Kind != "static" && site.Runtime.Application == nil {
				target := ""
				if site.Runtime.ProxyTarget != nil {
					target = *site.Runtime.ProxyTarget
				}
				if _, err = siteProxyURL(target); err != nil {
					problems = append(problems, err.Error())
				}
			}
			if len(problems) > 0 {
				r.issue("site-"+site.ID, "error", "站点「"+site.Name+"」配置有问题", strings.Join(problems, "；"), "/sites")
			}
		}
		state := "checked"
		if len(sites) == 0 {
			state = "skipped"
		}
		r.coverage("sites", "站点依赖与路径", state, fmt.Sprintf("%d 个站点；未请求站点或执行项目命令", len(sites)))
	}
	value, err := c.certHealth(ctx)
	if err != nil {
		r.unavailable("certificates", "证书", "/tls", err)
	} else {
		certs := value.(map[string]any)
		for _, cert := range certs["certs"].([]certHealth) {
			if cert.Status != "ok" || len(cert.MissingSANs) > 0 {
				severity := "warn"
				if cert.Status == "invalid" || cert.Status == "expired" {
					severity = "error"
				}
				r.issue("cert-"+cert.ID, severity, "证书「"+cert.Subject+"」需要处理", cert.Advice+"；状态："+cert.Status+"；缺少域名："+strings.Join(cert.MissingSANs, ", "), "/tls")
			}
		}
		local := false
		for _, site := range sites {
			local = local || site.HTTPS && site.Runtime.ImportedCertID == nil && site.Runtime.ACMECertID == nil
		}
		if local && !certs["caTrusted"].(bool) {
			r.issue("ca-untrusted", "warn", "本地 CA 尚未受信任", "本地 HTTPS 可能显示证书警告", "/tls")
		}
		r.coverage("certificates", "证书", "checked", "已检查证书文件、私钥配对、域名覆盖、有效期和本机信任；未访问公网证书")
	}
	if err = c.healthHosts(ctx); err != nil {
		r.unavailable("hosts", "hosts 托管映射", "/network", err)
	} else {
		r.coverage("hosts", "hosts 托管映射", "checked", "托管映射与站点及额外 hosts 设置一致；未测试系统 DNS")
	}
	for _, status := range c.serviceStatuses() {
		if status.LastError != nil {
			r.issue("service-"+status.ID, "error", status.Label+" 服务存在错误", status.LastError.Message+"；"+status.LastError.Detail, "/packages")
		}
		if len(status.MissingRequires) > 0 {
			r.issue("dependencies-"+status.ID, "warn", status.Label+" 缺少依赖", strings.Join(status.MissingRequires, ", "), "/packages")
		}
	}
	r.coverage("services", "受管进程状态", "checked", "读取本次应用管理的进程状态与依赖；不接管其他应用进程")
	if err = healthProbeDirectory(c.Home); err != nil {
		r.issue("data-directory", "error", "数据目录写入或清理失败", err.Error(), "/settings")
	}
	r.coverage("data-directory", "数据目录可写性", "checked", "创建、写入并清理唯一临时文件；未检查所有子目录")
	if !has["php"] {
		r.coverage("php-extensions", "PHP 扩展", "skipped", "未安装 PHP")
	}
	for _, p := range installed {
		if p["id"] != "php" {
			continue
		}
		version := p["version"].(string)
		id := "php-extensions-" + version
		view, err := c.phpExtensionView(ctx, version)
		if err != nil {
			r.unavailable(id, "PHP "+version+" 扩展", "/packages", err)
			continue
		}
		for _, ext := range view["extensions"].([]phpExtension) {
			if ext.Enabled && (!ext.Loaded || len(ext.MissingDeps) > 0) {
				r.issue(id+"-"+ext.Name, "warn", "PHP "+version+" 扩展 "+ext.Name+" 未正常加载", ext.Hint+"；缺少依赖："+strings.Join(ext.MissingDeps, ", "), "/packages")
			}
		}
		r.coverage(id, "PHP "+version+" 扩展", "checked", "使用所选版本 CLI 核对实际加载模块；不代表站点进程已重载")
	}
	r.coverage("application-probes", "配置语法与业务连通性", "skipped", "本次未运行原生配置校验、HTTP、数据库登录或 UDP；请使用服务诊断检查配置")
	r.finish(len(installed) == 0)
	return r, nil
}
func healthProbeDirectory(dir string) error {
	file, err := os.CreateTemp(dir, ".health-probe-")
	if err != nil {
		return err
	}
	_, writeErr := file.WriteString("NiceEnv health probe")
	return errors.Join(writeErr, file.Close(), os.Remove(file.Name()))
}
func (c *Core) healthHosts(ctx context.Context) error {
	f, err := c.readHostsFile()
	if err != nil {
		return err
	}
	actual, err := hostfile.Parse(f.Content, false)
	if err != nil {
		return err
	}
	wanted, err := c.hostSiteEntries(ctx)
	if err != nil {
		return err
	}
	extras, _, err := c.Store.setting(ctx, "extraHosts")
	if err != nil {
		return err
	}
	var pairs [][2]string
	if extras != "" {
		if err = json.Unmarshal([]byte(extras), &pairs); err != nil {
			return err
		}
	}
	expected := map[string]bool{}
	for _, entry := range wanted {
		expected[entry.IP+" "+entry.Domain] = true
	}
	for _, pair := range pairs {
		entry, err := hostfile.Normalize(pair[0], pair[1])
		if err != nil {
			return err
		}
		expected[entry.IP+" "+entry.Domain] = true
	}
	seen := map[string]bool{}
	for _, entry := range actual {
		if entry.Managed {
			seen[entry.IP+" "+entry.Domain] = true
		}
	}
	missing, extra := 0, 0
	for key := range expected {
		if !seen[key] {
			missing++
		}
	}
	for key := range seen {
		if !expected[key] {
			extra++
		}
	}
	if missing+extra > 0 {
		return &AppError{Code: "HOSTS_DRIFT", Message: fmt.Sprintf("hosts 托管映射缺少 %d 条、多出 %d 条，请核对后重建", missing, extra)}
	}
	return nil
}
func (c *Core) healthPorts(ctx context.Context, r *healthReport) {
	// Enumerate listeners once for the entire report, not once per service.
	connections, err := gnet.ConnectionsWithContext(ctx, "tcp")
	if err != nil {
		r.unavailable("ports", "端口监听", "/network", err)
		return
	}
	listeners := map[int]map[int]bool{}
	for _, conn := range connections {
		if conn.Status == "LISTEN" {
			port := int(conn.Laddr.Port)
			if listeners[port] == nil {
				listeners[port] = map[int]bool{}
			}
			listeners[port][int(conn.Pid)] = true
		}
	}
	type target struct {
		status ServiceStatus
		child  *process.Child
		pkg    Package
		port   int
	}
	targets := []target{}
	c.services.mu.RLock()
	for _, e := range c.services.entries {
		targets = append(targets, target{status: cloneStatus(e), child: e.child, pkg: e.pkg, port: e.status.Port})
	}
	c.services.mu.RUnlock()
	sort.Slice(targets, func(i, j int) bool { return targets[i].status.ID < targets[j].status.ID })
	count := 0
	unavailable := false
	for _, target := range targets {
		if target.child == nil && target.pkg.Run != nil && target.pkg.ID != "php" {
			port, err := c.servicePort(ctx, target.status.ID, target.pkg)
			if err != nil {
				r.unavailable("port-"+target.status.ID, "端口设置", "/network", err)
				unavailable = true
				continue
			}
			target.port = port
		}
		if target.port <= 0 {
			continue
		}
		ports := 1
		if target.pkg.ID == "php" {
			ports = phpPoolWorkers
		}
		for offset := 0; offset < ports; offset++ {
			port := target.port + offset
			count++
			owners := listeners[port]
			if len(owners) == 0 && target.child != nil {
				r.issue(fmt.Sprintf("port-missing-%s-%d", target.status.ID, port), "error", target.status.Label+" 未监听端口", fmt.Sprintf(":%d 尚未监听", port), "/packages")
			}
			for pid := range owners {
				owned := false
				var err error
				if target.child != nil {
					owned, err = target.child.OwnsPID(pid)
				}
				if err != nil || pid == 0 {
					unavailable = true
					r.issue(fmt.Sprintf("port-unknown-%d-%d", port, pid), "warn", "无法确认端口归属", fmt.Sprintf("%s :%d PID %d", target.status.Label, port, pid), "/network")
				} else if !owned {
					r.issue(fmt.Sprintf("port-conflict-%s-%d-%d", target.status.ID, port, pid), "warn", target.status.Label+" 端口被其他进程占用", fmt.Sprintf(":%d PID %d", port, pid), "/network")
				}
			}
		}
	}
	state := "checked"
	if unavailable {
		state = "unavailable"
	}
	r.coverage("ports", "TCP 主端口与进程归属", state, fmt.Sprintf("检查 %d 个服务端口及 PHP 池端口；未覆盖 HTTPS 等附加监听和 UDP", count))
}
