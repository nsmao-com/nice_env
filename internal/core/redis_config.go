package core

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"unicode"
)

type redisSaveRule struct {
	Seconds uint32 `json:"seconds"`
	Changes uint32 `json:"changes"`
}

func (c *Core) redisConfigRevision(key, content string) string {
	h := hmac.New(sha256.New, c.snapshotKey[:])
	h.Write([]byte(c.Home + "\x00" + key + "\x00" + content))
	return hex.EncodeToString(h.Sum(nil))
}

type redisSettings struct {
	MaxMemoryBytes *uint64          `json:"maxMemoryBytes"`
	EvictionPolicy *string          `json:"evictionPolicy"`
	TimeoutSeconds *uint32          `json:"timeoutSeconds"`
	MaxClients     *uint32          `json:"maxClients"`
	SaveRules      *[]redisSaveRule `json:"saveRules"`
	AppendFsync    *string          `json:"appendFsync"`
}

var redisSettingFields = []string{"maxmemory", "maxmemory-policy", "timeout", "maxclients", "save", "appendfsync", "appendonly"}

func redisDirective(line string) (string, []string, error) {
	text := strings.TrimSpace(line)
	if text == "" || strings.HasPrefix(text, "#") {
		return "", nil, nil
	}
	if text[0] == '\'' || text[0] == '"' {
		return "", nil, invalid("复杂 Redis 配置请使用配置编辑器")
	}
	words := strings.Fields(text)
	key := strings.ToLower(words[0])
	if key == "include" {
		return "", nil, invalid("配置包含 include，无法确认最终覆盖顺序，请使用配置编辑器")
	}
	if !slices.Contains(redisSettingFields, key) {
		return key, nil, nil
	}
	values := []string{}
	for _, word := range words[1:] {
		if strings.HasPrefix(word, "'") || strings.HasPrefix(word, "\"") {
			if len(word) < 2 || word[len(word)-1] != word[0] {
				return "", nil, invalid("Redis 配置参数无法由常用设置表示")
			}
			word = word[1 : len(word)-1]
		}
		if strings.ContainsAny(word, "'\"\\#") {
			return "", nil, invalid("Redis 配置参数无法由常用设置表示")
		}
		values = append(values, word)
	}
	return key, values, nil
}
func redisMemory(value string) (uint64, error) {
	value = strings.ToLower(value)
	i := 0
	for i < len(value) && value[i] >= '0' && value[i] <= '9' {
		i++
	}
	n, err := strconv.ParseUint(value[:i], 10, 64)
	if err != nil {
		return 0, invalid("Redis 内存设置无效")
	}
	factors := map[string]uint64{"": 1, "k": 1000, "kb": 1024, "m": 1000000, "mb": 1048576, "g": 1000000000, "gb": 1073741824}
	factor, ok := factors[value[i:]]
	if !ok || n > 9007199254740991/factor {
		return 0, invalid("Redis 内存设置无效")
	}
	return n * factor, nil
}
func parseRedisSettings(content string) (redisSettings, *bool, error) {
	var result redisSettings
	var appendOnly *bool
	for _, line := range strings.Split(content, "\n") {
		key, values, err := redisDirective(line)
		if err != nil {
			return result, nil, err
		}
		if !slices.Contains(redisSettingFields, key) {
			continue
		}
		if key == "save" {
			if result.SaveRules == nil {
				empty := []redisSaveRule{}
				result.SaveRules = &empty
			}
			if len(values) == 1 && values[0] == "" {
				*result.SaveRules = []redisSaveRule{}
				continue
			}
			if len(values) == 0 || len(values)%2 != 0 {
				return result, nil, invalid("Redis 快照规则格式无效")
			}
			for i := 0; i < len(values); i += 2 {
				s, e1 := strconv.ParseUint(values[i], 10, 31)
				n, e2 := strconv.ParseUint(values[i+1], 10, 31)
				if e1 != nil || e2 != nil || s == 0 {
					return result, nil, invalid("Redis 快照规则无效")
				}
				*result.SaveRules = append(*result.SaveRules, redisSaveRule{uint32(s), uint32(n)})
			}
			if len(*result.SaveRules) > 16 {
				return result, nil, invalid("Redis 快照规则不能超过 16 条")
			}
			continue
		}
		if len(values) != 1 {
			return result, nil, invalid("Redis 配置参数数量无效")
		}
		value := strings.ToLower(values[0])
		switch key {
		case "maxmemory":
			n, err := redisMemory(value)
			if err != nil {
				return result, nil, err
			}
			result.MaxMemoryBytes = &n
		case "maxmemory-policy":
			result.EvictionPolicy = &value
		case "appendfsync":
			result.AppendFsync = &value
		case "timeout", "maxclients":
			n, err := strconv.ParseUint(value, 10, 32)
			if err != nil {
				return result, nil, invalid("Redis 连接限制无效")
			}
			v := uint32(n)
			if key == "timeout" {
				result.TimeoutSeconds = &v
			} else {
				result.MaxClients = &v
			}
		case "appendonly":
			if value != "yes" && value != "no" {
				return result, nil, invalid("Redis AOF 设置无效")
			}
			flag := value == "yes"
			appendOnly = &flag
		}
	}
	return result, appendOnly, nil
}
func mergeRedisSettings(content string, next redisSettings, ack bool) (string, error) {
	before, _, err := parseRedisSettings(content)
	if err != nil {
		return "", err
	}
	if next.MaxMemoryBytes != nil && *next.MaxMemoryBytes > 9007199254740991 || next.TimeoutSeconds != nil && *next.TimeoutSeconds > 2147483647 || next.MaxClients != nil && (*next.MaxClients == 0 || *next.MaxClients > 2147483647) {
		return "", invalid("Redis 内存或连接限制无效")
	}
	if next.SaveRules != nil {
		if len(*next.SaveRules) > 16 {
			return "", invalid("最多支持 16 条快照规则")
		}
		for _, r := range *next.SaveRules {
			if r.Seconds == 0 || r.Seconds > 2147483647 || r.Changes > 2147483647 {
				return "", invalid("快照规则数值无效")
			}
		}
		if len(*next.SaveRules) == 0 && (before.SaveRules == nil || len(*before.SaveRules) > 0) && !ack {
			return "", invalid("关闭自动快照前请确认数据丢失风险")
		}
	}
	if !reflect.DeepEqual(next.EvictionPolicy, before.EvictionPolicy) && next.EvictionPolicy != nil && !slices.Contains([]string{"noeviction", "allkeys-lru", "allkeys-lfu", "allkeys-random", "volatile-lru", "volatile-lfu", "volatile-random", "volatile-ttl"}, *next.EvictionPolicy) {
		return "", invalid("Redis 淘汰策略无效")
	}
	if !reflect.DeepEqual(next.AppendFsync, before.AppendFsync) && next.AppendFsync != nil && !slices.Contains([]string{"always", "everysec", "no"}, *next.AppendFsync) {
		return "", invalid("Redis AOF 同步策略无效")
	}
	edits := map[string][]string{}
	for _, field := range []struct {
		key       string
		old, next any
	}{{"maxmemory", before.MaxMemoryBytes, next.MaxMemoryBytes}, {"maxmemory-policy", before.EvictionPolicy, next.EvictionPolicy}, {"timeout", before.TimeoutSeconds, next.TimeoutSeconds}, {"maxclients", before.MaxClients, next.MaxClients}, {"appendfsync", before.AppendFsync, next.AppendFsync}} {
		if reflect.DeepEqual(field.old, field.next) {
			continue
		}
		value := reflect.ValueOf(field.next)
		edits[field.key] = []string{}
		if !value.IsNil() {
			edits[field.key] = []string{field.key + " " + fmt.Sprint(value.Elem().Interface())}
		}
	}
	if !reflect.DeepEqual(before.SaveRules, next.SaveRules) {
		edits["save"] = []string{}
		if next.SaveRules != nil {
			if len(*next.SaveRules) == 0 {
				edits["save"] = []string{"save \"\""}
			} else {
				for _, r := range *next.SaveRules {
					edits["save"] = append(edits["save"], fmt.Sprintf("save %d %d", r.Seconds, r.Changes))
				}
			}
		}
	}
	merged := redisMergeLines(content, edits)
	actual, _, err := parseRedisSettings(merged)
	if err != nil {
		return "", err
	}
	if !reflect.DeepEqual(actual, next) {
		return "", invalid("Redis 配置合并结果不一致，未写入")
	}
	return merged, nil
}
func redisMergeLines(content string, edits map[string][]string) string {
	newline := "\n"
	if strings.Contains(content, "\r\n") {
		newline = "\r\n"
	}
	written := map[string]bool{}
	var out strings.Builder
	for _, line := range strings.SplitAfter(content, "\n") {
		fields := strings.Fields(line)
		key := ""
		if len(fields) > 0 {
			key = strings.ToLower(fields[0])
		}
		if replacements, ok := edits[key]; ok {
			if !written[key] {
				for _, replacement := range replacements {
					out.WriteString(replacement + newline)
				}
				written[key] = true
			}
			continue
		}
		out.WriteString(line)
	}
	keys := []string{}
	for key := range edits {
		keys = append(keys, key)
	}
	slices.Sort(keys)
	for _, key := range keys {
		if !written[key] && len(edits[key]) > 0 {
			if out.Len() > 0 && !strings.HasSuffix(out.String(), "\n") {
				out.WriteString(newline)
			}
			for _, replacement := range edits[key] {
				out.WriteString(replacement + newline)
			}
		}
	}
	return out.String()
}
func redisPasswordState(content string) (bool, error) {
	enabled := false
	for _, line := range strings.Split(content, "\n") {
		text := strings.TrimSpace(line)
		if text == "" || text[0] == '#' {
			continue
		}
		fields := strings.Fields(text)
		key := strings.ToLower(fields[0])
		value := strings.TrimSpace(text[len(fields[0]):])
		if key == "include" || key == "user" || key == "aclfile" && value != "\"\"" && value != "''" {
			return false, invalid("配置包含 include 或 ACL 规则，请在配置编辑器管理认证")
		}
		if key != "requirepass" {
			continue
		}
		if value == "" {
			return false, invalid("密码配置格式无效")
		}
		if value[0] == '"' || value[0] == '\'' {
			quote := value[0]
			i := 1
			for ; i < len(value); i++ {
				if value[i] == '\\' && (quote == '"' || i+1 < len(value) && value[i+1] == '\'') {
					i++
					continue
				}
				if value[i] == quote {
					break
				}
			}
			if i != len(value)-1 || value[i] != quote {
				return false, invalid("密码配置格式复杂，请使用配置编辑器")
			}
			enabled = i > 1
		} else {
			if strings.ContainsAny(value, " \t\"'") {
				return false, invalid("密码配置格式复杂，请使用配置编辑器")
			}
			enabled = true
		}
	}
	return enabled, nil
}
func (c *Core) redisConfigCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Version            string        `json:"version"`
		Revision           string        `json:"revision"`
		Password           string        `json:"password"`
		AcknowledgeDisable bool          `json:"acknowledgeDisable"`
		Settings           redisSettings `json:"settings"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !safePart(in.Version) {
		return nil, invalid("请选择 Redis 版本")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	target, err := c.configTarget(ctx, "redis-conf@"+in.Version)
	if err != nil {
		return nil, err
	}
	if action == "stop" {
		e, err := c.selectedRedis(in.Version)
		if err != nil {
			return nil, err
		}
		return nil, c.stopOwned(ctx, e, false)
	}
	content, err := readConfigText(target.path)
	if err != nil {
		return nil, err
	}
	revision := c.redisConfigRevision(target.key, content)
	passwordView := func(text string) map[string]any {
		enabled, err := redisPasswordState(text)
		var blocked any
		if err != nil {
			blocked = err.Error()
		}
		return map[string]any{"version": in.Version, "revision": c.redisConfigRevision(target.key, text), "enabled": enabled, "blockedReason": blocked}
	}
	settingsView := func(text string) (any, error) {
		settings, aof, err := parseRedisSettings(text)
		if err != nil {
			return nil, err
		}
		return map[string]any{"version": in.Version, "path": Path(target.path), "revision": c.redisConfigRevision(target.key, text), "settings": settings, "appendOnly": aof}, nil
	}
	switch action {
	case "password":
		return passwordView(content), nil
	case "settings":
		return settingsView(content)
	}
	if in.Revision == "" || in.Revision != revision {
		return nil, invalid("Redis 配置已变化，请重新读取后保存")
	}
	if action == "password-save" {
		e, err := c.selectedRedis(in.Version)
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		running := e.child != nil
		c.services.mu.RUnlock()
		if running {
			return nil, invalid("请先正常停止 Redis，再修改启动密码")
		}
		if _, err = redisPasswordState(content); err != nil {
			return nil, err
		}
		if len(in.Password) > 512 || strings.IndexFunc(in.Password, unicode.IsControl) >= 0 || in.Password != "" && strings.TrimSpace(in.Password) == "" {
			return nil, invalid("密码不能超过 512 字节、仅为空白或含控制字符")
		}
		if in.Password == "" && !in.AcknowledgeDisable {
			return nil, invalid("关闭密码认证前请确认")
		}
		quoted := strings.NewReplacer("\\", "\\\\", "\"", "\\\"").Replace(in.Password)
		merged := redisMergeLines(content, map[string][]string{"requirepass": {"requirepass \"" + quoted + "\""}})
		if _, err = redisPasswordState(merged); err != nil {
			return nil, err
		}
		if err = c.writeConfigExpected(ctx, target, merged, &content, false); err != nil {
			return nil, err
		}
		err = c.Store.saveSetting(ctx, "redisConnection@"+in.Version, redisCredentials{Password: in.Password})
		return map[string]any{"view": passwordView(merged), "connectionSaved": err == nil}, nil
	}
	if action == "settings-save" {
		merged, err := mergeRedisSettings(content, in.Settings, in.AcknowledgeDisable)
		if err != nil {
			return nil, err
		}
		if err = c.writeConfigExpected(ctx, target, merged, &content, false); err != nil {
			return nil, err
		}
		return settingsView(merged)
	}
	return nil, invalid("未知 Redis 配置操作")
}
