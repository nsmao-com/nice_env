package core

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"niceenv/internal/process"
)

const phpPoolWorkers = 4

func phpIniQuote(path string) (string, error) {
	value := filepath.ToSlash(DisplayPath(path))
	if strings.ContainsAny(value, "\r\n\x00\"") || strings.Contains(value, "${") {
		return "", invalid("PHP 配置路径含无法安全表达的字符")
	}
	return `"` + value + `"`, nil
}

func (c *Core) preparePHP(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	if err := c.ensurePHPIni(ctx, e, r); err != nil {
		return err
	}
	lockDir := filepath.Join(c.Home, "service-locks")
	unlock, err := process.Lock(filepath.Join(lockDir, "php-pool-allocation.lock"))
	if err != nil {
		return err
	}
	defer unlock()
	rows, err := c.Store.db.QueryContext(ctx, "SELECT service_id, base_port FROM port_assign")
	if err != nil {
		return err
	}
	used := map[int]bool{}
	for rows.Next() {
		var id string
		var port int
		if err = rows.Scan(&id, &port); err != nil {
			rows.Close()
			return err
		}
		if id == e.status.ID {
			continue
		}
		count := 1
		if strings.HasPrefix(id, "php@") {
			count = phpPoolWorkers
		}
		for offset := 0; offset < count; offset++ {
			used[port+offset] = true
		}
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return err
	}
	override, fixed, err := c.Store.setting(ctx, "portOverride."+e.status.ID)
	if err != nil {
		return err
	}
	base := r.port
	if fixed {
		base, err = strconv.Atoi(override)
		if err != nil {
			return invalid("PHP 池端口无效")
		}
	}
	pool := func(base int) []int {
		if base < 1 || base > 65535-phpPoolWorkers+1 {
			return nil
		}
		ports := make([]int, phpPoolWorkers)
		for offset := range ports {
			ports[offset] = base + offset
			if used[base+offset] {
				return nil
			}
		}
		return ports
	}
	ports := pool(base)
	if len(ports) == 0 || checkServicePorts(ctx, ports) != nil {
		if fixed {
			return &AppError{Code: "PHP_POOL_PORT_CONFLICT", Message: "指定的 PHP 四端口池存在冲突，请调整端口"}
		}
		ports = nil
		for candidate := 9100; candidate+phpPoolWorkers <= 9200; candidate += phpPoolWorkers {
			proposed := pool(candidate)
			if len(proposed) > 0 && checkServicePorts(ctx, proposed) == nil {
				base, ports = candidate, proposed
				break
			}
		}
	}
	if len(ports) != phpPoolWorkers {
		return &AppError{Code: "NO_FREE_PORT", Message: "PHP 端口池 9100–9199 没有可用的连续四端口"}
	}
	if _, err = c.Store.db.ExecContext(ctx, "INSERT INTO port_assign(service_id,base_port) VALUES(?,?) ON CONFLICT(service_id) DO UPDATE SET base_port=excluded.base_port", e.status.ID, base); err != nil {
		return err
	}
	r.port, r.ports = base, ports
	r.env["PHPRC"] = DisplayPath(r.etc)
	r.env["PHP_INI_SCAN_DIR"] = ""
	// A worker exiting after N requests would take its entire owned pool down.
	// Lifecycle and memory recycling are explicit service restarts instead.
	r.env["PHP_FCGI_MAX_REQUESTS"] = "0"
	c.services.mu.Lock()
	e.status.Port = base
	c.services.mu.Unlock()
	return nil
}

func (c *Core) ensurePHPIni(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	path := filepath.Join(r.etc, "php.ini")
	if _, err := readConfigText(path); err == nil {
		return nil
	} else if !os.IsNotExist(err) {
		return err
	}
	sessionDir := filepath.Join(r.data, "sess")
	if err := managedDirectory(c.Home, sessionDir); err != nil {
		return err
	}
	text, err := defaultPHPIni(e, r)
	if err != nil {
		return err
	}
	target := configTarget{spec: configSpecs[3], key: "php-ini@" + e.pkg.Version, path: path, program: r.program, version: e.pkg.Version}
	return c.writeConfigExpected(ctx, target, text, nil, true)
}

func defaultPHPIni(e *serviceEntry, r *resolvedService) (string, error) {
	sessionDir := filepath.Join(r.data, "sess")
	values := make([]string, 3)
	for i, path := range []string{filepath.Join(filepath.Dir(r.program), "ext"), filepath.Join(filepath.Dir(r.log), "php_errors.log"), sessionDir} {
		var err error
		values[i], err = phpIniQuote(path)
		if err != nil {
			return "", err
		}
	}
	text := fmt.Sprintf("; NiceEnv managed php.ini (%s)\n[PHP]\nengine=On\nexpose_php=Off\nmemory_limit=256M\nerror_reporting=E_ALL\ndisplay_errors=On\ndisplay_startup_errors=On\nlog_errors=On\nextension_dir=%s\nerror_log=%s\nmax_execution_time=300\nmax_input_time=300\npost_max_size=128M\nupload_max_filesize=128M\ndefault_charset=\"UTF-8\"\ncgi.force_redirect=0\ncgi.fix_pathinfo=1\nfastcgi.impersonate=1\nfastcgi.logging=0\nvariables_order=\"EGPCS\"\nrequest_order=\"GP\"\ndate.timezone=Asia/Shanghai\nopcache.enable=1\nopcache.enable_cli=0\n[Session]\nsession.save_handler=files\nsession.save_path=%s\n[Extensions]\n", e.pkg.Version, values[0], values[1], values[2])
	for _, name := range []string{"curl", "fileinfo", "gd", "gd2", "mbstring", "mysqli", "openssl", "pdo_mysql", "sockets"} {
		for _, filename := range []string{"php_" + name + ".dll", name + ".so"} {
			if info, err := os.Stat(filepath.Join(filepath.Dir(r.program), "ext", filename)); err == nil && info.Mode().IsRegular() {
				text += "extension=" + name + "\n"
				break
			}
		}
	}
	return text, nil
}
