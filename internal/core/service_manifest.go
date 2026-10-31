package core

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"time"

	"niceenv/internal/process"
)

func (c *Core) prepareManifestService(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	if isMySQLEngine(e.pkg.ID) {
		return nil
	}
	switch e.pkg.ID {
	case "php", "nginx", "redis", "apache", "caddy", "postgresql", "coredns", "qdrant", "mongodb", "mihomo", "rabbitmq", "tomcat", "neo4j", "elasticsearch", "sftpgo", "minio", "consul":
		return nil
	}
	run := e.pkg.Run
	if run == nil {
		return nil
	}
	if run.ConfigTemplate != "" {
		path, err := relativePath(r.etc, run.ConfigFile)
		if err != nil {
			return err
		}
		if info, err := os.Lstat(path); err == nil {
			if !info.Mode().IsRegular() {
				return invalid("服务配置不是普通文件")
			}
		} else if !os.IsNotExist(err) {
			return err
		} else {
			content, err := r.expand(run.ConfigTemplate)
			if err != nil {
				return err
			}
			if err = c.writeConfigExpected(ctx, configTarget{key: e.pkg.ID + "-" + e.pkg.Version, path: path}, content, nil, true); err != nil {
				return err
			}
		}
	}
	if len(run.InitArgs) == 0 {
		return nil
	}
	marker := run.InitMarker
	if marker == "" {
		marker = ".niceenv-initialized"
	}
	path, err := relativePath(r.data, marker)
	if err != nil {
		return err
	}
	if info, err := os.Lstat(path); err == nil {
		if !info.Mode().IsRegular() {
			return invalid("服务初始化标记无效")
		}
		return nil
	} else if !os.IsNotExist(err) {
		return err
	}
	program := r.program
	if run.InitBin != "" {
		program, err = relativePath(filepath.Dir(r.program), run.InitBin)
		if err != nil {
			return err
		}
	}
	args, err := r.expandArgs(run.InitArgs)
	if err != nil {
		return err
	}
	resolved, err := filepath.EvalSymlinks(program)
	if err != nil {
		return err
	}
	if !pathWithin(filepath.Dir(r.program), resolved) {
		return invalid("初始化程序超出已安装程序目录")
	}
	if info, err := os.Stat(resolved); err != nil {
		return err
	} else if !info.Mode().IsRegular() {
		return invalid("初始化程序不是普通文件")
	}
	pending := filepath.Join(r.data, ".niceenv-init-in-progress")
	file, err := os.OpenFile(pending, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if os.IsExist(err) {
		return &AppError{Code: "SERVICE_INIT_INTERRUPTED", Message: "上次服务初始化未完成，未再次执行初始化命令", Hint: "请核对数据目录和服务日志，恢复或移走不完整数据后再试。"}
	}
	if err != nil {
		return err
	}
	if err = file.Close(); err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, 2*time.Minute)
	defer cancel()
	out := process.NewLimitedBuffer(32768)
	child, err := process.Start(ctx, process.Spec{Program: program, Args: args, Directory: r.cwd, Environment: r.env, RemoveEnvironment: r.removeEnvironment, Stdout: out, Stderr: out})
	if err != nil {
		return errors.Join(err, os.Remove(pending))
	}
	result := child.Wait()
	if result.Err != nil || result.Code != 0 {
		return &AppError{Code: "SERVICE_INIT_FAILED", Message: "服务初始化失败，已保留中断标记以避免重复修改数据", Detail: redactDiagnostic(out.String())}
	}
	if err = managedDirectory(c.Home, filepath.Dir(path)); err != nil {
		return err
	}
	if err = atomicWrite(path, []byte("initialized\n")); err != nil {
		return err
	}
	return errors.Join(ctx.Err(), os.Remove(pending))
}
