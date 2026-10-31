package core

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync"

	"github.com/go-acme/lego/v5/challenge/dns01"
)

// Legacy DNSPod uses login_token=id,token; Tencent SecretId/SecretKey is a
// different API. Retain only IDs created by this provider instance for cleanup.
type dnsPodRecord struct{ Zone, ID string }
type legacyDNSPod struct {
	client  *http.Client
	token   string
	mu      sync.Mutex
	records map[string]dnsPodRecord
}

func (p *legacyDNSPod) call(ctx context.Context, action string, values url.Values) (map[string]json.RawMessage, error) {
	values.Set("login_token", p.token)
	values.Set("format", "json")
	values.Set("lang", "en")
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, "https://dnsapi.cn/"+action, strings.NewReader(values.Encode()))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("User-Agent", "NiceEnv ACME DNS01")
	resp, err := p.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("DNSPod %s 请求未完成", action)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("DNSPod 返回 HTTP %d", resp.StatusCode)
	}
	var body map[string]json.RawMessage
	if err = json.NewDecoder(io.LimitReader(resp.Body, 1024*1024)).Decode(&body); err != nil {
		return nil, invalid("DNSPod 响应格式无效")
	}
	var status struct {
		Code string `json:"code"`
	}
	if json.Unmarshal(body["status"], &status) != nil || status.Code != "1" {
		return nil, fmt.Errorf("DNSPod %s 失败（状态 %s），请检查权限与域名", action, status.Code)
	}
	return body, nil
}
func (p *legacyDNSPod) Present(ctx context.Context, domain, token, keyAuth string) error {
	info := dns01.GetChallengeInfo(ctx, domain, keyAuth)
	zone, err := dns01.DefaultClient().FindZoneByFqdn(ctx, info.EffectiveFQDN)
	if err != nil {
		return err
	}
	zone = strings.TrimSuffix(zone, ".")
	name := strings.TrimSuffix(strings.TrimSuffix(info.EffectiveFQDN, "."), "."+zone)
	if name == zone {
		name = "@"
	}
	body, err := p.call(ctx, "Record.Create", url.Values{"domain": {zone}, "sub_domain": {name}, "record_type": {"TXT"}, "record_line": {"默认"}, "value": {info.Value}, "ttl": {"600"}})
	if err != nil {
		return err
	}
	var record struct {
		ID string `json:"id"`
	}
	if json.Unmarshal(body["record"], &record) != nil || record.ID == "" {
		return invalid("DNSPod 未确认新建记录 ID，请检查 DNS 控制台")
	}
	p.mu.Lock()
	p.records[domain+"\x00"+keyAuth] = dnsPodRecord{zone, record.ID}
	p.mu.Unlock()
	return nil
}
func (p *legacyDNSPod) CleanUp(ctx context.Context, domain, token, keyAuth string) error {
	key := domain + "\x00" + keyAuth
	p.mu.Lock()
	record, ok := p.records[key]
	p.mu.Unlock()
	if !ok {
		return nil
	}
	_, err := p.call(ctx, "Record.Remove", url.Values{"domain": {record.Zone}, "record_id": {record.ID}})
	if err == nil {
		p.mu.Lock()
		delete(p.records, key)
		p.mu.Unlock()
	}
	return err
}
