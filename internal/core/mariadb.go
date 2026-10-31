package core

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"

	"niceenv/internal/process"
)

func (c *Core) prepareMariaDB(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	base := filepath.Dir(filepath.Dir(r.program))
	r.cwd = base
	config := filepath.Join(r.etc, "my.ini")
	if _, err := readConfigText(config); os.IsNotExist(err) {
		baseText, err := phpIniQuote(base)
		if err != nil {
			return err
		}
		dataText, err := phpIniQuote(r.data)
		if err != nil {
			return err
		}
		text := fmt.Sprintf("[mysqld]\nbasedir=%s\ndatadir=%s\nport=%d\nbind-address=127.0.0.1\ncharacter-set-server=utf8mb4\ncollation-server=utf8mb4_unicode_ci\nmax_connections=150\ninnodb_buffer_pool_size=128M\n[client]\nport=%d\ndefault-character-set=utf8mb4\n", baseText, dataText, r.port, r.port)
		target := configTarget{spec: configSpecs[5], key: "mariadb-ini@" + e.pkg.Version, path: config, program: r.program, version: e.pkg.Version}
		if err = c.writeConfigExpected(ctx, target, text, nil, true); err != nil {
			return err
		}
	} else if err != nil {
		return err
	}
	r.args = []string{"--defaults-file=" + DisplayPath(config), "--basedir=" + DisplayPath(base), "--datadir=" + DisplayPath(r.data), "--port=" + strconv.Itoa(r.port), "--bind-address=127.0.0.1"}
	if runtime.GOOS == "windows" {
		r.args = append(r.args, "--console")
	}
	r.stopArgs = nil
	files, err := os.ReadDir(r.data)
	if err != nil {
		return err
	}
	if len(files) > 0 {
		version, err := readConfigText(filepath.Join(r.data, ".niceenv-mariadb-version"))
		if err != nil || canonicalVersion(strings.TrimSpace(version)) != canonicalVersion(e.pkg.Version) {
			return &AppError{Code: "MARIADB_DATA_VERSION", Message: "MariaDB 数据目录版本无法确认或与程序不同，未启动或升级"}
		}
		marker, err := readConfigText(filepath.Join(r.data, ".niceenv-bootstrap"))
		if err != nil && !os.IsNotExist(err) {
			return err
		}
		saved, _, err := c.Store.setting(ctx, mysqlBootstrapKey(e))
		if err != nil {
			return err
		}
		r.freshDatabase = marker != "" && marker == saved
		return nil
	}
	stage, err := os.MkdirTemp(filepath.Dir(r.data), ".mariadb-init-")
	if err != nil {
		return err
	}
	program := filepath.Join(filepath.Dir(r.program), "mariadb-install-db.exe")
	args := []string{"--datadir=" + DisplayPath(stage), "--port=" + strconv.Itoa(r.port)}
	if runtime.GOOS != "windows" {
		program = filepath.Join(base, "scripts", "mariadb-install-db")
		if _, err = os.Stat(program); os.IsNotExist(err) {
			program = filepath.Join(base, "bin", "mariadb-install-db")
		}
		args = []string{"--no-defaults", "--basedir=" + base, "--datadir=" + stage, "--auth-root-authentication-method=normal", "--skip-test-db"}
	}
	info, err := os.Lstat(program)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() {
		return invalid("MariaDB 初始化工具不是普通文件")
	}
	initCtx, cancel := context.WithTimeout(ctx, 3*time.Minute)
	defer cancel()
	output := process.NewLimitedBuffer(64 * 1024)
	child, err := process.Start(initCtx, process.Spec{Program: program, Directory: base, Args: args, Stdout: output, Stderr: output, RemoveEnvironment: []string{"MYSQL_HOME", "MARIADB_HOME", "MYSQL_PWD", "MYSQL_HOST", "MYSQL_TCP_PORT"}})
	if err != nil {
		return err
	}
	result := child.Wait()
	if result.Err != nil || result.Code != 0 || initCtx.Err() != nil {
		return &AppError{Code: "MARIADB_INIT_FAILED", Message: "MariaDB 初始化失败，原数据目录未覆盖", Detail: redactDiagnostic(output.String())}
	}
	if info, err = os.Stat(filepath.Join(stage, "mysql")); err != nil || !info.IsDir() {
		return invalid("MariaDB 初始化未生成系统库，未发布数据目录")
	}
	identity, err := databaseRandomPassword()
	if err != nil {
		return err
	}
	if err = os.WriteFile(filepath.Join(stage, ".niceenv-mariadb-version"), []byte(e.pkg.Version), 0600); err != nil {
		return err
	}
	if err = os.WriteFile(filepath.Join(stage, ".niceenv-bootstrap"), []byte(identity), 0600); err != nil {
		return err
	}
	if err = c.Store.saveSetting(ctx, mysqlBootstrapKey(e), identity); err != nil {
		return err
	}
	if err = os.Remove(r.data); err != nil {
		return err
	}
	if err = os.Rename(stage, r.data); err != nil {
		return err
	}
	r.freshDatabase = true
	return nil
}
