package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	gprocess "github.com/shirou/gopsutil/v4/process"
	"niceenv/internal/process"
)

const productionMarker = ".niceenv-wails-data"
const productionSignature = "NiceEnv Go data version 2\n"

// PrepareBrowserHome only prepares the browser storage location. Legacy schema
// inspection, snapshots and service discovery run in Bridge's background startup.
func PrepareBrowserHome(ctx context.Context, home string) error {
	if BuildChannel != "production" {
		return PrepareDevelopmentHome(home)
	}
	if err := validateMoveDirectory(home); err != nil {
		return err
	}
	if info, err := os.Lstat(filepath.Join(home, "nsb.sqlite")); err == nil {
		if !info.Mode().IsRegular() {
			return invalid("应用数据库不是普通文件")
		}
		return nil
	} else if !os.IsNotExist(err) {
		return err
	}
	return PrepareApplicationHome(ctx, home)
}

func PrepareApplicationHome(ctx context.Context, home string) error {
	unlock, err := acquireApplicationHome(ctx, home)
	if err != nil {
		return err
	}
	return unlock()
}

func acquireApplicationHome(ctx context.Context, home string) (func() error, error) {
	if BuildChannel != "production" {
		if err := PrepareDevelopmentHome(home); err != nil {
			return nil, err
		}
	} else {
		if err := validateMoveDirectory(home); err != nil {
			return nil, err
		}
		if err := os.MkdirAll(home, 0700); err != nil {
			return nil, err
		}
	}
	var locks []func() error
	release := func() error {
		var err error
		for i := len(locks) - 1; i >= 0; i-- {
			err = errors.Join(err, locks[i]())
		}
		return err
	}
	// The legacy lock names deliberately remain: v1 and v2 cannot own the same
	// databases concurrently, even if the old application is opened again.
	for _, name := range []string{".niceenv-wails-control.lock", ".niceenv-control.lock", ".data-dir-activity.lock"} {
		path := filepath.Join(home, name)
		if info, err := os.Lstat(path); err == nil && !info.Mode().IsRegular() {
			_ = release()
			return nil, invalid("数据目录锁文件不是普通文件")
		} else if err != nil && !os.IsNotExist(err) {
			_ = release()
			return nil, err
		}
		unlock, err := process.Lock(path)
		if err != nil {
			_ = release()
			return nil, &AppError{Code: "CONTROLLER_BUSY", Message: "此数据目录正在被另一个 NiceEnv 会话使用，请先退出旧版", Detail: err.Error()}
		}
		locks = append(locks, unlock)
	}
	if BuildChannel == "production" {
		if err := prepareProductionHome(ctx, home); err != nil {
			_ = release()
			return nil, err
		}
	}
	return release, nil
}

func prepareProductionHome(ctx context.Context, home string) error {
	marker := filepath.Join(home, productionMarker)
	// Respect a v1 data-directory handover rather than reviving a retired copy.
	if data, err := readPrivateMaterial(filepath.Join(home, ".data-dir-authority.json")); err == nil {
		var authority struct {
			Version   int    `json:"version"`
			Selection string `json:"selection_file"`
		}
		if json.Unmarshal(data, &authority) != nil || authority.Version != 1 || !filepath.IsAbs(authority.Selection) {
			return invalid("旧版数据目录交接记录无效")
		}
		selected, err := readLegacyDataSelection(authority.Selection)
		if err != nil {
			return err
		}
		if selected != "" && !sameLocalPath(selected, home) {
			return &AppError{Code: "DATA_DIR_RELOCATED", Message: "此目录已由旧版迁移，请使用当前数据目录", Hint: DisplayPath(selected)}
		}
	} else if !os.IsNotExist(err) {
		return err
	}
	if _, err := os.Lstat(filepath.Join(home, ".data-dir-activation.json")); err == nil {
		return invalid("旧版数据迁移尚未完成，请先在旧版完成重启后再升级")
	} else if !os.IsNotExist(err) {
		return err
	}
	if data, err := readPrivateMaterial(marker); err == nil {
		if string(data) != productionSignature {
			return invalid("Go 数据目录标记无效")
		}
		return nil
	} else if !os.IsNotExist(err) {
		return err
	}
	database := filepath.Join(home, "nsb.sqlite")
	info, err := os.Lstat(database)
	if os.IsNotExist(err) {
		entries, err := os.ReadDir(home)
		if err != nil {
			return err
		}
		for _, entry := range entries {
			switch entry.Name() {
			case ".niceenv-wails-control.lock", ".niceenv-control.lock", ".data-dir-activity.lock":
			default:
				return invalid("所选非空目录没有 NiceEnv 数据库，未将其初始化为新环境")
			}
		}
		return writePrivateAtomic(marker, []byte(productionSignature))
	}
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() {
		return invalid("应用数据库不是普通文件")
	}
	if err := ensureLegacyServicesStopped(ctx, home); err != nil {
		return err
	}
	path := filepath.ToSlash(DisplayPath(database))
	if !strings.HasPrefix(path, "/") {
		path = "/" + path
	}
	u := url.URL{Scheme: "file", Path: path}
	q := u.Query()
	q.Set("mode", "ro")
	q.Set("_pragma", "busy_timeout(5000)")
	u.RawQuery = q.Encode()
	db, err := sql.Open("sqlite", u.String())
	if err != nil {
		return err
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	if err = validateLegacyStore(ctx, db); err != nil {
		return err
	}
	backup := filepath.Join(home, "backup", "upgrade-v2")
	if err = managedDirectory(home, backup); err != nil {
		return err
	}
	id, err := newID("metadata")
	if err != nil {
		return err
	}
	snapshot := filepath.Join(backup, id+".sqlite")
	// VACUUM INTO captures committed WAL data as well; a file copy could lose it.
	if _, err = db.ExecContext(ctx, "VACUUM main INTO ?", DisplayPath(snapshot)); err != nil {
		return err
	}
	if err = process.PrivateFile(snapshot); err != nil {
		return err
	}
	return writePrivateAtomic(marker, []byte(productionSignature))
}

func sameLocalPath(a, b string) bool {
	a = filepath.Clean(DisplayPath(a))
	b = filepath.Clean(DisplayPath(b))
	if runtime.GOOS == "windows" {
		return strings.EqualFold(a, b)
	}
	return a == b
}

func storeTableColumns(ctx context.Context, db *sql.DB, table string) (map[string]bool, error) {
	// table comes exclusively from the fixed schema list below or openStore.
	rows, err := db.QueryContext(ctx, "PRAGMA table_info("+table+")")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	result := map[string]bool{}
	for rows.Next() {
		var id, required, primary int
		var name, typ string
		var defaultValue any
		if err := rows.Scan(&id, &name, &typ, &required, &defaultValue, &primary); err != nil {
			return nil, err
		}
		result[name] = true
	}
	return result, rows.Err()
}

func validateLegacyStore(ctx context.Context, db *sql.DB) error {
	rows, err := db.QueryContext(ctx, "SELECT name,type FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
	if err != nil {
		return err
	}
	tables := map[string]string{}
	for rows.Next() {
		var name, kind string
		if err = rows.Scan(&name, &kind); err != nil {
			rows.Close()
			return err
		}
		tables[name] = kind
	}
	err = errors.Join(rows.Err(), rows.Close())
	if err != nil {
		return err
	}
	for table, fields := range map[string]string{
		"settings": "key value", "installed": "key id version category install_path config_path installed_at",
		"sites":          "id name domains root_dir runtime https rewrite db created_at updated_at",
		"certs":          "id kind subject sans not_before not_after cert_path key_path",
		"proxy_profiles": "id name url active added_at", "port_assign": "service_id base_port",
		"stacks":           "id name description items builtin created_at updated_at",
		"cert_automations": "id data updated_at", "cert_monitors": "id data updated_at",
		"cron_jobs": "id name command interval_min enabled created_at last_run_at last_exit last_output",
	} {
		if tables[table] == "" && table != "settings" && table != "installed" && table != "sites" {
			continue
		}
		if tables[table] != "table" {
			return invalid("旧版数据库缺少有效数据表：" + table)
		}
		columns, err := storeTableColumns(ctx, db, table)
		if err != nil {
			return err
		}
		for _, field := range strings.Fields(fields) {
			if !columns[field] {
				return invalid("旧版数据库字段不兼容：" + table + "." + field)
			}
		}
	}
	var integrity string
	if err = db.QueryRowContext(ctx, "PRAGMA quick_check").Scan(&integrity); err != nil {
		return err
	}
	if integrity != "ok" {
		return invalid("旧版应用数据库完整性检查失败，未修改数据")
	}
	return nil
}

func ensureLegacyServicesStopped(ctx context.Context, home string) error {
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	list, err := gprocess.ProcessesWithContext(ctx)
	if err != nil {
		return err
	}
	root := filepath.Join(home, "runtimes")
	for _, p := range list {
		if err := ctx.Err(); err != nil {
			return err
		}
		if p.Pid == int32(os.Getpid()) {
			continue
		}
		exe, err := p.ExeWithContext(ctx)
		if err != nil || exe == "" {
			continue
		}
		rel, err := filepath.Rel(root, DisplayPath(exe))
		if err == nil && filepath.IsLocal(rel) {
			return &AppError{Code: "LEGACY_SERVICE_RUNNING", Message: "旧版服务仍在运行，请先在旧版停止服务后升级", Detail: fmt.Sprintf("PID %d · %s", p.Pid, DisplayPath(exe))}
		}
	}
	return nil
}
