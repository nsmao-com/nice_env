package core

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

type redisCredentials struct {
	Username string `json:"username"`
	Password string `json:"password"`
}
type redisClient struct {
	conn   net.Conn
	reader *bufio.Reader
	ctx    context.Context
	info   map[string]string
}

func (r *redisClient) close() { r.conn.Close() }
func (r *redisClient) command(args ...string) (any, error) {
	deadline := time.Now().Add(5 * time.Second)
	if d, ok := r.ctx.Deadline(); ok && d.Before(deadline) {
		deadline = d
	}
	if err := r.conn.SetDeadline(deadline); err != nil {
		return nil, err
	}
	if err := r.ctx.Err(); err != nil {
		return nil, err
	}
	var b strings.Builder
	fmt.Fprintf(&b, "*%d\r\n", len(args))
	for _, arg := range args {
		fmt.Fprintf(&b, "$%d\r\n%s\r\n", len(arg), arg)
	}
	if _, err := io.WriteString(r.conn, b.String()); err != nil {
		return nil, err
	}
	budget := 16 * 1024 * 1024
	return redisReply(r.reader, 0, &budget)
}
func redisReply(reader *bufio.Reader, depth int, budget *int) (any, error) {
	if depth > 8 || *budget <= 0 {
		return nil, invalid("Redis 响应超过限制")
	}
	line, err := reader.ReadSlice('\n')
	if err != nil {
		return nil, err
	}
	*budget -= len(line)
	if len(line) < 3 || line[len(line)-2] != '\r' {
		return nil, invalid("Redis 响应格式无效")
	}
	kind := line[0]
	value := string(line[1 : len(line)-2])
	switch kind {
	case '+':
		return value, nil
	case '-':
		return nil, &AppError{Code: "REDIS_COMMAND_FAILED", Message: "Redis 拒绝操作，请检查认证、权限和运行状态", Detail: redactDiagnostic(value)}
	case ':':
		return strconv.ParseInt(value, 10, 64)
	case '$':
		n, err := strconv.Atoi(value)
		if err != nil || n < -1 || n > *budget {
			return nil, invalid("Redis 返回值过大或长度无效")
		}
		if n == -1 {
			return nil, nil
		}
		*budget -= n + 2
		data := make([]byte, n+2)
		if _, err = io.ReadFull(reader, data); err != nil {
			return nil, err
		}
		if data[n] != '\r' || data[n+1] != '\n' {
			return nil, invalid("Redis 响应未正确结束")
		}
		return string(data[:n]), nil
	case '*':
		n, err := strconv.Atoi(value)
		if err != nil || n < -1 || n > 20000 {
			return nil, invalid("Redis 数组长度无效")
		}
		if n == -1 {
			return nil, nil
		}
		out := make([]any, 0, n)
		for i := 0; i < n; i++ {
			item, err := redisReply(reader, depth+1, budget)
			if err != nil {
				return nil, err
			}
			out = append(out, item)
		}
		return out, nil
	}
	return nil, invalid("Redis 响应类型不支持")
}
func redisInfo(value any) (map[string]string, error) {
	text, ok := value.(string)
	if !ok {
		return nil, invalid("Redis INFO 响应无效")
	}
	out := map[string]string{}
	for _, line := range strings.Split(text, "\n") {
		key, val, ok := strings.Cut(strings.TrimSpace(line), ":")
		if ok {
			out[key] = val
		}
	}
	return out, nil
}
func (c *Core) redisCredentials(ctx context.Context, e *serviceEntry) (redisCredentials, error) {
	var result redisCredentials
	value, found, err := c.Store.setting(ctx, "redisConnection@"+e.pkg.Version)
	if err != nil {
		return result, err
	}
	if found && json.Unmarshal([]byte(value), &result) != nil {
		return result, invalid("Redis 连接记录损坏，请重新保存")
	}
	return result, nil
}
func validateRedisCredentials(value redisCredentials) error {
	if len(value.Username) > 512 || len(value.Password) > 16384 || strings.ContainsRune(value.Username+value.Password, 0) || value.Username != "" && value.Password == "" {
		return invalid("Redis 用户名或密码无效；无认证时两项均留空")
	}
	return nil
}
func (c *Core) redisConnect(ctx context.Context, e *serviceEntry, credentials *redisCredentials) (*redisClient, error) {
	if err := c.verifyDatabaseListener(ctx, e); err != nil {
		return nil, err
	}
	var creds redisCredentials
	var err error
	if credentials == nil {
		creds, err = c.redisCredentials(ctx, e)
		if err != nil {
			return nil, err
		}
	} else {
		creds = *credentials
	}
	if err = validateRedisCredentials(creds); err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	port, child := e.resolved.port, e.child
	program, configPath := e.resolved.program, filepath.Join(e.resolved.etc, "redis.conf")
	c.services.mu.RUnlock()
	conn, err := (&net.Dialer{Timeout: 3 * time.Second}).DialContext(ctx, "tcp4", net.JoinHostPort("127.0.0.1", strconv.Itoa(port)))
	if err != nil {
		return nil, err
	}
	r := &redisClient{conn: conn, reader: bufio.NewReaderSize(conn, 8192), ctx: ctx}
	success := false
	defer func() {
		if !success {
			r.close()
		}
	}()
	if creds.Password != "" {
		args := []string{"AUTH"}
		if creds.Username != "" {
			args = append(args, creds.Username)
		}
		args = append(args, creds.Password)
		if _, err = r.command(args...); err != nil {
			return nil, &AppError{Code: "REDIS_AUTH_FAILED", Message: "Redis 认证失败，请检查保存的凭据"}
		}
	}
	response, err := r.command("INFO", "server")
	if err != nil {
		return nil, err
	}
	r.info, err = redisInfo(response)
	if err != nil {
		return nil, err
	}
	pid, err := strconv.Atoi(r.info["process_id"])
	if err != nil {
		return nil, invalid("Redis 未返回有效进程号")
	}
	if runtime.GOOS == "windows" && redisPOSIX(program) {
		// MSYS reports its emulated POSIX PID. Verify the native listener again
		// and bind INFO to this exact configuration before exposing the OS PID.
		reported := r.info["config_file"]
		if strings.HasPrefix(reported, "/cygdrive/") && len(reported) > 11 {
			reported = reported[10:11] + ":" + reported[11:]
		}
		if !strings.EqualFold(filepath.Clean(filepath.FromSlash(reported)), filepath.Clean(configPath)) {
			return nil, invalid("Redis 返回的配置路径与当前实例不符")
		}
		if err = c.verifyDatabaseListener(ctx, e); err != nil {
			return nil, err
		}
		pid = child.PID
		r.info["process_id"] = strconv.Itoa(pid)
	}
	owned, err := child.OwnsPID(pid)
	if err != nil {
		return nil, err
	}
	if !owned {
		return nil, invalid("Redis 响应与受管实例不符")
	}
	if len(r.info["run_id"]) != 40 {
		return nil, invalid("Redis 实例标识无效")
	}
	success = true
	return r, nil
}
func (c *Core) selectedRedis(version string) (*serviceEntry, error) {
	return c.serviceEntry("redis", version)
}
func redisPOSIX(program string) bool {
	if runtime.GOOS != "windows" {
		return true
	}
	for _, dll := range []string{"msys-2.0.dll", "cygwin1.dll"} {
		if info, err := os.Stat(filepath.Join(filepath.Dir(program), dll)); err == nil && info.Mode().IsRegular() {
			return true
		}
	}
	return false
}
func (c *Core) prepareRedis(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	path := filepath.Join(r.etc, "redis.conf")
	text, err := readConfigText(path)
	if os.IsNotExist(err) {
		dir, err := phpIniQuote(r.data)
		if err != nil {
			return err
		}
		text = fmt.Sprintf("# NiceEnv Redis configuration\nbind 127.0.0.1\nprotected-mode yes\nport %d\ndaemonize no\ndatabases 16\nsave \"\"\nappendonly no\nmaxmemory 256mb\nmaxmemory-policy allkeys-lru\ndir %s\ndbfilename dump.rdb\nlogfile \"\"\n", r.port, dir)
		target := configTarget{spec: configSpecs[6], key: "redis-conf@" + e.pkg.Version, path: path, program: r.program, version: e.pkg.Version}
		if err = c.writeConfigExpected(ctx, target, text, nil, true); err != nil {
			return err
		}
	} else if err != nil {
		return err
	}
	// CLI operands are passed separately and override includes without rewriting
	// passwords or user configuration text. Persistent data remains shared as in v1.
	posix := redisPOSIX(r.program)
	pathArg := func(value string) string {
		value = DisplayPath(value)
		if !posix {
			return value
		}
		value = strings.ReplaceAll(value, "\\", "/")
		if len(value) > 2 && value[1] == ':' {
			return "/cygdrive/" + strings.ToLower(value[:1]) + value[2:]
		}
		return value
	}
	config := pathArg(path)
	major, _ := strconv.Atoi(strings.Split(canonicalVersion(e.pkg.Version), ".")[0])
	if posix && major >= 7 && strings.ContainsAny(config, "*?[") {
		config = strings.NewReplacer("\\", "\\\\", "*", "\\*", "?", "\\?", "[", "\\[").Replace(config)
	}
	r.args = []string{config, "--port", strconv.Itoa(r.port), "--dir", pathArg(r.data), "--daemonize", "no", "--logfile", ""}
	r.stopArgs = nil
	return nil
}
func (c *Core) shutdownRedis(ctx context.Context, e *serviceEntry) error {
	client, err := c.redisConnect(ctx, e, nil)
	if err != nil {
		return err
	}
	defer client.close()
	_, err = client.command("SHUTDOWN", "SAVE")
	if err == io.EOF || err == io.ErrUnexpectedEOF {
		return nil
	}
	return err
}

type redisRequest struct {
	Version      string  `json:"version"`
	Database     int     `json:"database"`
	Cursor       string  `json:"cursor"`
	Pattern      string  `json:"pattern"`
	Count        int     `json:"count"`
	Key          string  `json:"key"`
	Value        *string `json:"value"`
	TTLMode      string  `json:"ttlMode"`
	TTLMS        int64   `json:"ttlMs"`
	Revision     string  `json:"revision"`
	Confirmation string  `json:"confirmation"`
}

func (c *Core) registerRedis() {
	c.handle("redis_settings", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisConfigCommand(ctx, data, "settings")
	})
	c.handle("redis_settings_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisConfigCommand(ctx, data, "settings-save")
	})
	c.handle("redis_password", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisConfigCommand(ctx, data, "password")
	})
	c.handle("redis_password_stop", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisConfigCommand(ctx, data, "stop")
	})
	c.handle("redis_password_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisConfigCommand(ctx, data, "password-save")
	})
	c.handle("redis_connection", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "connection")
	})
	c.handle("redis_save_connection", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "save-connection")
	})
	c.handle("redis_stats", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "stats")
	})
	c.handle("redis_keys", func(ctx context.Context, data json.RawMessage) (any, error) { return c.redisCommand(ctx, data, "keys") })
	c.handle("redis_key_preview", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "preview")
	})
	c.handle("redis_key_update", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "update")
	})
	c.handle("redis_key_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "delete")
	})
	c.handle("redis_flush", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "flush")
	})
	c.handle("redis_persistence", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "persistence")
	})
	c.handle("redis_snapshot", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.redisCommand(ctx, data, "snapshot")
	})
}
func (c *Core) redisCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Version     string           `json:"version"`
		Credentials redisCredentials `json:"credentials"`
		Request     redisRequest     `json:"request"`
	}](data)
	if err != nil {
		return nil, err
	}
	request := in.Request
	if request.Version == "" {
		request.Version = in.Version
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	e, err := c.selectedRedis(request.Version)
	if err != nil {
		return nil, err
	}
	request.Version = e.pkg.Version
	if action == "connection" {
		creds, err := c.redisCredentials(ctx, e)
		return map[string]any{"version": e.pkg.Version, "username": creds.Username, "hasPassword": creds.Password != ""}, err
	}
	var credentials *redisCredentials
	if action == "save-connection" {
		credentials = &in.Credentials
	}
	client, err := c.redisConnect(ctx, e, credentials)
	if err != nil {
		return nil, err
	}
	defer client.close()
	if action == "stats" || action == "save-connection" {
		result, err := redisStats(client, e.status.Port)
		if err != nil {
			return nil, err
		}
		if action == "save-connection" {
			err = c.Store.saveSetting(ctx, "redisConnection@"+e.pkg.Version, in.Credentials)
		}
		return result, err
	}
	if action == "persistence" || action == "snapshot" {
		info, err := redisPersistence(client, e.pkg.Version)
		if err != nil {
			return nil, err
		}
		if action == "persistence" {
			return info, nil
		}
		if info["loading"] == true || info["saving"] == true || info["aofRewriting"] == true {
			return nil, invalid("Redis 正在加载或持久化，请稍后创建快照")
		}
		now, err := client.command("TIME")
		if err != nil {
			return nil, err
		}
		parts, ok := now.([]any)
		if !ok || len(parts) != 2 {
			return nil, invalid("Redis TIME 响应无效")
		}
		seconds, err := strconv.ParseInt(fmt.Sprint(parts[0]), 10, 64)
		if err != nil {
			return nil, err
		}
		result, err := client.command("BGSAVE")
		if err != nil {
			return nil, err
		}
		if result != "Background saving started" {
			return nil, invalid("Redis 未确认后台快照开始")
		}
		return map[string]any{"version": e.pkg.Version, "runId": info["runId"], "processId": info["processId"], "minimumSaveTime": seconds}, nil
	}
	if request.Database < 0 || request.Database > 1023 {
		return nil, invalid("Redis 逻辑库编号无效")
	}
	if _, err = client.command("SELECT", strconv.Itoa(request.Database)); err != nil {
		return nil, err
	}
	if action == "keys" {
		return redisKeys(client, request)
	}
	if action == "flush" {
		if strings.TrimSpace(request.Confirmation) != "FLUSHDB" {
			return nil, invalid("请输入 FLUSHDB 确认清空当前逻辑库")
		}
		reply, err := client.command("FLUSHDB", "ASYNC")
		if err != nil {
			return nil, err
		}
		if reply != "OK" {
			return nil, invalid("Redis 未确认清空请求")
		}
		return map[string]any{"version": request.Version, "database": request.Database, "mode": "async"}, nil
	}
	if request.Key == "" || len(request.Key) > 4096 || strings.ContainsRune(request.Key, 0) {
		return nil, invalid("键名无效或过长")
	}
	if action == "delete" {
		if request.Confirmation != request.Key {
			return nil, invalid("请输入完整键名确认删除")
		}
		n, err := client.command("DEL", request.Key)
		if err != nil {
			return nil, err
		}
		if n != int64(1) {
			return nil, invalid("键已不存在，请刷新列表")
		}
		return map[string]any{"version": request.Version, "database": request.Database, "key": request.Key, "deleted": 1}, nil
	}
	if action == "preview" {
		if _, err = client.command("WATCH", request.Key); err != nil {
			return nil, err
		}
		preview, err := c.redisPreview(client, e, request)
		if err != nil {
			return nil, err
		}
		if _, err = client.command("MULTI"); err != nil {
			return nil, err
		}
		if _, err = client.command("PING"); err != nil {
			return nil, err
		}
		result, err := client.command("EXEC")
		if err != nil {
			return nil, err
		}
		if result == nil {
			return nil, invalid("键在读取期间发生变化，请刷新")
		}
		return preview, nil
	}
	if action == "update" {
		return c.redisUpdate(client, e, request)
	}
	return nil, invalid("未知 Redis 操作")
}
func redisStats(client *redisClient, port int) (any, error) {
	reply, err := client.command("INFO")
	if err != nil {
		return nil, err
	}
	info, err := redisInfo(reply)
	if err != nil {
		return nil, err
	}
	keys := int64(0)
	for key, value := range info {
		if strings.HasPrefix(key, "db") {
			for _, part := range strings.Split(value, ",") {
				if strings.HasPrefix(part, "keys=") {
					n, _ := strconv.ParseInt(strings.TrimPrefix(part, "keys="), 10, 64)
					keys += n
				}
			}
		}
	}
	uptime, _ := strconv.ParseInt(info["uptime_in_days"], 10, 64)
	clients, _ := strconv.ParseInt(info["connected_clients"], 10, 64)
	return map[string]any{"reachable": true, "port": port, "usedMemoryHuman": info["used_memory_human"], "keys": keys, "uptimeDays": uptime, "connectedClients": clients}, nil
}
func redisPersistence(client *redisClient, version string) (map[string]any, error) {
	reply, err := client.command("INFO", "persistence")
	if err != nil {
		return nil, err
	}
	info, err := redisInfo(reply)
	if err != nil {
		return nil, err
	}
	pid, err := strconv.Atoi(client.info["process_id"])
	if err != nil {
		return nil, err
	}
	out := map[string]any{"version": version, "runId": client.info["run_id"], "processId": pid, "lastSaveDuration": nil, "aofRewriteScheduled": nil, "aofLastRewriteStatus": nil, "aofLastWriteStatus": nil}
	for source, target := range map[string]string{"loading": "loading", "rdb_bgsave_in_progress": "saving", "aof_enabled": "aofEnabled", "aof_rewrite_in_progress": "aofRewriting"} {
		value := info[source]
		if value != "0" && value != "1" {
			return nil, invalid("Redis 持久化状态不完整")
		}
		out[target] = value == "1"
	}
	for source, target := range map[string]string{"rdb_changes_since_last_save": "changesSinceSave", "rdb_last_save_time": "lastSaveTime"} {
		n, err := strconv.ParseInt(info[source], 10, 64)
		if err != nil || n < 0 {
			return nil, invalid("Redis 持久化计数无效")
		}
		out[target] = n
	}
	status := info["rdb_last_bgsave_status"]
	if status != "ok" && status != "err" {
		return nil, invalid("Redis 快照状态无效")
	}
	out["lastSaveStatus"] = status
	if n, err := strconv.ParseInt(info["rdb_last_bgsave_time_sec"], 10, 64); err == nil && n >= 0 {
		out["lastSaveDuration"] = n
	}
	if v, ok := info["aof_rewrite_scheduled"]; ok {
		out["aofRewriteScheduled"] = v == "1"
	}
	for source, target := range map[string]string{"aof_last_bgrewrite_status": "aofLastRewriteStatus", "aof_last_write_status": "aofLastWriteStatus"} {
		if v, ok := info[source]; ok {
			if v != "ok" && v != "err" {
				return nil, invalid("Redis AOF 状态无效")
			}
			out[target] = v
		}
	}
	return out, nil
}
func redisKeys(client *redisClient, r redisRequest) (any, error) {
	if r.Cursor == "" {
		r.Cursor = "0"
	}
	if _, err := strconv.ParseUint(r.Cursor, 10, 64); err != nil {
		return nil, invalid("SCAN 游标无效")
	}
	if len(r.Pattern) > 4096 {
		return nil, invalid("匹配条件过长")
	}
	if r.Pattern == "" {
		r.Pattern = "*"
	}
	if r.Count == 0 {
		r.Count = 100
	}
	if r.Count < 1 || r.Count > 500 {
		return nil, invalid("扫描数量须为 1–500")
	}
	reply, err := client.command("SCAN", r.Cursor, "MATCH", r.Pattern, "COUNT", strconv.Itoa(r.Count))
	if err != nil {
		return nil, err
	}
	parts, ok := reply.([]any)
	if !ok || len(parts) != 2 {
		return nil, invalid("SCAN 响应无效")
	}
	next, ok := parts[0].(string)
	if !ok {
		return nil, invalid("SCAN 游标无效")
	}
	keys, ok := parts[1].([]any)
	if !ok || len(keys) > 2000 {
		return nil, invalid("单次扫描结果过多，请缩小范围")
	}
	items := []map[string]any{}
	for _, value := range keys {
		key, ok := value.(string)
		if !ok || !utf8.ValidString(key) {
			continue
		}
		kind, err := client.command("TYPE", key)
		if err != nil {
			return nil, err
		}
		ttl, err := client.command("PTTL", key)
		if err != nil {
			return nil, err
		}
		if kind == "none" {
			continue
		}
		items = append(items, map[string]any{"key": key, "keyType": kind, "ttlMs": ttl})
	}
	return map[string]any{"version": r.Version, "database": r.Database, "cursor": r.Cursor, "nextCursor": next, "pattern": r.Pattern, "items": items}, nil
}
func (c *Core) redisPreview(client *redisClient, e *serviceEntry, r redisRequest) (map[string]any, error) {
	kind, err := client.command("TYPE", r.Key)
	if err != nil {
		return nil, err
	}
	if kind == "none" {
		return nil, invalid("键已不存在")
	}
	ttl, err := client.command("PTTL", r.Key)
	if err != nil {
		return nil, err
	}
	out := map[string]any{"version": r.Version, "database": r.Database, "key": r.Key, "keyType": kind, "ttlMs": ttl, "valueTruncated": false}
	if size, err := client.command("MEMORY", "USAGE", r.Key); err == nil && size != nil {
		out["memoryBytes"] = size
	}
	if kind == "string" {
		size, err := client.command("STRLEN", r.Key)
		if err != nil {
			return nil, err
		}
		out["elements"] = size
		value, err := client.command("GETRANGE", r.Key, "0", "65535")
		if err != nil {
			return nil, err
		}
		text, ok := value.(string)
		if !ok {
			return nil, invalid("Redis 字符串响应无效")
		}
		length, ok := size.(int64)
		if !ok {
			return nil, invalid("Redis 字符串长度响应无效")
		}
		truncated := length > 65536 || !utf8.ValidString(text)
		if !utf8.ValidString(text) {
			text = strings.ToValidUTF8(text, "�")
		}
		out["value"] = text
		out["valueTruncated"] = truncated
	} else {
		commands := map[any]string{"hash": "HLEN", "list": "LLEN", "set": "SCARD", "zset": "ZCARD", "stream": "XLEN"}
		if command := commands[kind]; command != "" {
			n, err := client.command(command, r.Key)
			if err != nil {
				return nil, err
			}
			out["elements"] = n
		}
	}
	// DUMP gives a content fingerprint for every data type; it is never returned.
	dump, err := client.command("DUMP", r.Key)
	if err != nil {
		return nil, err
	}
	if dump == nil {
		return nil, invalid("键已过期")
	}
	dumpText, ok := dump.(string)
	if !ok {
		return nil, invalid("Redis 内容快照无效")
	}
	revision, err := c.accountRevision(e, []any{client.info["run_id"], r.Database, r.Key, kind, []byte(dumpText)})
	if err != nil {
		return nil, err
	}
	out["revision"] = revision
	return out, nil
}
func (c *Core) redisUpdate(client *redisClient, e *serviceEntry, r redisRequest) (any, error) {
	if r.Revision == "" {
		return nil, invalid("请重新读取键详情")
	}
	if r.TTLMode != "preserve" && r.TTLMode != "persist" && r.TTLMode != "duration" {
		return nil, invalid("过期设置无效")
	}
	if r.TTLMode == "duration" && (r.TTLMS < 1 || r.TTLMS > 31536000000000) {
		return nil, invalid("过期时间必须为 1 毫秒至 1000 年")
	}
	if r.Value != nil && len(*r.Value) > 1024*1024 {
		return nil, invalid("字符串值不能超过 1 MiB")
	}
	if r.Value == nil && r.TTLMode == "preserve" {
		return nil, invalid("没有需要保存的变化")
	}
	if _, err := client.command("WATCH", r.Key); err != nil {
		return nil, err
	}
	before, err := c.redisPreview(client, e, r)
	if err != nil {
		return nil, err
	}
	if before["revision"] != r.Revision {
		return nil, invalid("键内容已变化，请重新读取")
	}
	if r.Value != nil && before["keyType"] != "string" {
		return nil, invalid("仅字符串键支持编辑值")
	}
	// WATCH/MULTI prevents an external writer from changing data after revision
	// validation. Lua preserves the actual remaining TTL at execution time.
	script := `if redis.call('EXISTS',KEYS[1])==0 then return 0 end
local ttl=redis.call('PTTL',KEYS[1])
if ARGV[1]=='1' then redis.call('SET',KEYS[1],ARGV[2],'XX') end
if ARGV[3]=='duration' then redis.call('PEXPIRE',KEYS[1],ARGV[4])
elseif ARGV[3]=='persist' then redis.call('PERSIST',KEYS[1])
elseif ARGV[1]=='1' and ttl>0 then redis.call('PEXPIRE',KEYS[1],ttl) end
return 1`
	hasValue, value := "0", ""
	if r.Value != nil {
		hasValue, value = "1", *r.Value
	}
	if _, err = client.command("MULTI"); err != nil {
		return nil, err
	}
	if _, err = client.command("EVAL", script, "1", r.Key, hasValue, value, r.TTLMode, strconv.FormatInt(r.TTLMS, 10)); err != nil {
		return nil, err
	}
	reply, err := client.command("EXEC")
	if err != nil {
		return nil, err
	}
	items, ok := reply.([]any)
	if !ok || len(items) != 1 || items[0] != int64(1) {
		return nil, invalid("键在保存期间已变化或过期，请重新读取")
	}
	return map[string]any{"version": r.Version, "database": r.Database, "key": r.Key, "updated": true}, nil
}
