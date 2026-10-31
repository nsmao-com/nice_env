package core

import (
	"context"
	"crypto/hmac"
	"crypto/md5"
	"crypto/sha1"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"path"
	"slices"
	"strconv"
	"strings"
	"time"
)

func validateCertDeployTarget(t certDeployTarget) error {
	required := map[string][]string{"local": {"certPath", "keyPath"}, "ssh": {"host", "user", "hostFingerprint", "certPath", "keyPath"}, "btpanel": {"url", "apiSk", "siteName"}, "onepanel": {"url", "token"}, "aliyun": {"accessKeyId", "accessKeySecret"}, "tencent": {"secretId", "secretKey"}}
	keys, ok := required[t.Kind]
	if !ok {
		return invalid("不支持此证书部署类型")
	}
	for _, key := range keys {
		if strings.TrimSpace(t.Config[key]) == "" {
			return invalid("证书部署缺少配置：" + key)
		}
	}
	if _, err := certDeployTimeout(t); err != nil {
		return err
	}
	if t.Kind == "btpanel" || t.Kind == "onepanel" {
		u, err := url.Parse(t.Config["url"])
		if err != nil || u.Hostname() == "" || (u.Scheme != "https" && u.Scheme != "http") || u.User != nil || u.RawQuery != "" || u.Fragment != "" {
			return invalid("面板地址须为完整 HTTP/HTTPS 地址，不含凭据或查询参数")
		}
	}
	if t.Kind == "local" {
		a, err := certOutputPath(t.Config["certPath"], false)
		if err != nil {
			return err
		}
		b, err := certOutputPath(t.Config["keyPath"], false)
		if err != nil {
			return err
		}
		if strings.EqualFold(a, b) {
			return invalid("证书和私钥路径不能相同")
		}
		t.Config["certPath"], t.Config["keyPath"] = a, b
	}
	if t.Kind == "ssh" {
		port, err := certSSHPort(t)
		if err != nil {
			return err
		}
		if _, _, err = sshEndpoint(t.Config["host"], port); err != nil {
			return err
		}
		fp := strings.TrimPrefix(t.Config["hostFingerprint"], "SHA256:")
		decoded, e := base64.RawStdEncoding.DecodeString(fp)
		if e != nil || len(decoded) != 32 || !strings.HasPrefix(t.Config["hostFingerprint"], "SHA256:") {
			return invalid("请先读取并确认 SSH 主机 SHA256 指纹")
		}
		for _, key := range []string{"certPath", "keyPath"} {
			p := t.Config[key]
			if !strings.HasPrefix(p, "/") || p == "/" || path.Clean(p) != p || strings.ContainsAny(p, "\x00\r\n\\") {
				return invalid("远程证书路径必须是无 . 或 .. 的绝对路径")
			}
		}
		if t.Config["certPath"] == t.Config["keyPath"] {
			return invalid("远程证书和私钥路径重复")
		}
		if t.Config["auth"] == "key" {
			if certSSHIdentity(t) == "" {
				return invalid("请选择 SSH 登录私钥文件")
			}
		} else if t.Config["auth"] != "" && t.Config["auth"] != "password" {
			return invalid("SSH 认证方式无效")
		} else if t.Config["password"] == "" {
			return invalid("请填写 SSH 登录密码")
		}
	}
	if region := t.Config["region"]; region != "" {
		for _, ch := range region {
			if !(ch >= 'a' && ch <= 'z' || ch >= '0' && ch <= '9' || ch == '-') {
				return invalid("云服务区域无效")
			}
		}
	}
	return nil
}
func certSSHPort(t certDeployTarget) (int, error) {
	if t.Config["port"] == "" {
		return 22, nil
	}
	p, err := strconv.Atoi(t.Config["port"])
	if err != nil || p < 1 || p > 65535 {
		return 0, invalid("SSH 端口无效")
	}
	return p, nil
}
func certSSHIdentity(t certDeployTarget) string {
	if t.Config["identityFile"] != "" {
		return t.Config["identityFile"]
	}
	return t.Config["privateKey"]
}
func certMD5(s string) string    { sum := md5.Sum([]byte(s)); return hex.EncodeToString(sum[:]) }
func certSHA256(s string) string { sum := sha256.Sum256([]byte(s)); return hex.EncodeToString(sum[:]) }
func certHMAC(key []byte, s string) []byte {
	h := hmac.New(sha256.New, key)
	h.Write([]byte(s))
	return h.Sum(nil)
}
func certPercent(s string) string { return strings.ReplaceAll(url.QueryEscape(s), "+", "%20") }

// Never include response bodies or request URLs in persisted errors: panel
// responses can echo API credentials or uploaded private material.
func certDeployPOST(ctx context.Context, endpoint, contentType, body string, headers map[string]string) (map[string]json.RawMessage, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, strings.NewReader(body))
	if err != nil {
		return nil, invalid("部署地址无效")
	}
	req.Header.Set("Content-Type", contentType)
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	transport := outboundTransport()
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 45 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	resp, err := client.Do(req)
	if err != nil {
		return nil, &AppError{Code: "DEPLOY_UNCERTAIN", Message: "部署请求未收到确认，请核对目标端后手动重试"}
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("部署端点返回 HTTP %d", resp.StatusCode)
	}
	var result map[string]json.RawMessage
	if err = json.NewDecoder(io.LimitReader(resp.Body, 2*1024*1024)).Decode(&result); err != nil {
		return nil, &AppError{Code: "DEPLOY_UNCERTAIN", Message: "部署端点未返回有效确认，请核对目标端"}
	}
	return result, nil
}
func certUploadID(raw json.RawMessage) string {
	var s string
	if json.Unmarshal(raw, &s) == nil && s != "" {
		return s
	}
	var n json.Number
	if json.Unmarshal(raw, &n) == nil {
		if _, err := n.Int64(); err == nil {
			return n.String()
		}
	}
	return ""
}

func (c *Core) deployCertTarget(ctx context.Context, t certDeployTarget, m certIssuedMaterial) (string, error) {
	if err := validateCertDeployTarget(t); err != nil {
		return "", err
	}
	switch t.Kind {
	case "local":
		return c.deployLocalTarget(ctx, t, m)
	case "ssh":
		return deployCertSSH(ctx, t, m)
	case "btpanel":
		ts := strconv.FormatInt(time.Now().Unix(), 10)
		sha := sha1.Sum([]byte(ts + certMD5(t.Config["apiSk"])))
		token := certMD5(hex.EncodeToString(sha[:]))
		base := strings.TrimRight(t.Config["url"], "/")
		ssl, _ := json.Marshal(map[string]any{"type": 1, "key": m.KeyPEM, "csr": m.Chain})
		values := url.Values{"request_time": {ts}, "request_token": {token}, "siteName": {t.Config["siteName"]}, "ssl": {string(ssl)}}
		if t.Config["forceHTTPS"] == "true" {
			values.Set("forceHTTPS", "1")
		}
		body, err := certDeployPOST(ctx, base+"/site?action=SetSSL", "application/x-www-form-urlencoded", values.Encode(), nil)
		if err != nil {
			return "", err
		}
		var ok bool
		if json.Unmarshal(body["status"], &ok) != nil || !ok {
			return "", invalid("宝塔未确认 SSL 部署，请检查 API 权限和站点名称")
		}
		return "宝塔站点证书已更新", nil
	case "onepanel":
		ts := strconv.FormatInt(time.Now().Unix(), 10)
		payload, _ := json.Marshal(map[string]any{"ssl": map[string]any{"proxy": "", "privateKey": m.KeyPEM, "certificate": m.Chain, "description": m.Domains[0], "provider": "manual", "type": "included", "autoRenew": false, "domains": []string{}}})
		body, err := certDeployPOST(ctx, strings.TrimRight(t.Config["url"], "/")+"/api/v2/websites/ssl/upload", "application/json", string(payload), map[string]string{"1Panel-Token": certMD5("1panel" + ts + t.Config["token"]), "1Panel-Timestamp": ts})
		if err != nil {
			return "", err
		}
		var code int
		if json.Unmarshal(body["code"], &code) != nil || code != 200 {
			return "", invalid("1Panel 未确认上传，请检查 API 设置")
		}
		return "已上传到 1Panel 证书库", nil
	case "aliyun":
		return deployCertAliyun(ctx, t, m)
	case "tencent":
		return deployCertTencent(ctx, t, m)
	}
	return "", invalid("证书部署类型无效")
}
func deployCertAliyun(ctx context.Context, t certDeployTarget, m certIssuedMaterial) (string, error) {
	name := t.Config["certName"]
	if name == "" {
		name = "NiceEnv"
	}
	name = string([]rune(name)[:min(50, len([]rune(name)))]) + "-" + certSHA256(m.Chain)[:12]
	nonce, err := newID("request")
	if err != nil {
		return "", err
	}
	params := url.Values{"Format": {"JSON"}, "Version": {"2020-04-07"}, "AccessKeyId": {t.Config["accessKeyId"]}, "SignatureMethod": {"HMAC-SHA1"}, "SignatureVersion": {"1.0"}, "SignatureNonce": {nonce}, "Timestamp": {time.Now().UTC().Format(time.RFC3339)}, "Action": {"UploadUserCertificate"}, "Name": {name}, "ClientToken": {certSHA256(name + "\n" + m.Chain)}, "Cert": {m.Chain}, "Key": {m.KeyPEM}}
	query := strings.ReplaceAll(params.Encode(), "+", "%20")
	mac := hmac.New(sha1.New, []byte(t.Config["accessKeySecret"]+"&"))
	mac.Write([]byte("POST&%2F&" + certPercent(query)))
	signature := base64.StdEncoding.EncodeToString(mac.Sum(nil))
	region := t.Config["region"]
	if region == "" {
		region = "cn-hangzhou"
	}
	host := "cas." + region + ".aliyuncs.com"
	if strings.HasPrefix(region, "cn-") || slices.Contains([]string{"ap-northeast-2-pop", "ap-southeast-3", "ap-southeast-5", "eu-west-1", "eu-west-1-oxs", "rus-west-1-pop", "us-east-1", "us-west-1"}, region) {
		host = "cas.aliyuncs.com"
	}
	body, err := certDeployPOST(ctx, "https://"+host+"/", "application/x-www-form-urlencoded", "Signature="+certPercent(signature)+"&"+query, nil)
	if err != nil {
		return "", err
	}
	if _, ok := body["Code"]; ok {
		return "", invalid("阿里云未接受证书，请检查 CAS 权限和区域")
	}
	id := certUploadID(body["CertId"])
	if id == "" {
		return "", &AppError{Code: "DEPLOY_UNCERTAIN", Message: "阿里云未返回证书 ID，请核对证书库"}
	}
	return "已上传到阿里云证书库（" + id + "）", nil
}
func deployCertTencent(ctx context.Context, t certDeployTarget, m certIssuedMaterial) (string, error) {
	name := t.Config["certName"]
	if name == "" {
		name = "NiceEnv"
	}
	payload, _ := json.Marshal(map[string]any{"CertificatePublicKey": m.Chain, "CertificatePrivateKey": m.KeyPEM, "CertificateType": "SVR", "Repeatable": false, "Alias": name})
	now := time.Now().UTC()
	date := now.Format("2006-01-02")
	ts := strconv.FormatInt(now.Unix(), 10)
	host := "ssl.tencentcloudapi.com"
	signed := "content-type;host;x-tc-action"
	canonical := "POST\n/\n\ncontent-type:application/json; charset=utf-8\nhost:" + host + "\nx-tc-action:uploadcertificate\n\n" + signed + "\n" + certSHA256(string(payload))
	scope := date + "/ssl/tc3_request"
	toSign := "TC3-HMAC-SHA256\n" + ts + "\n" + scope + "\n" + certSHA256(canonical)
	key := certHMAC([]byte("TC3"+t.Config["secretKey"]), date)
	key = certHMAC(key, "ssl")
	key = certHMAC(key, "tc3_request")
	auth := "TC3-HMAC-SHA256 Credential=" + t.Config["secretId"] + "/" + scope + ", SignedHeaders=" + signed + ", Signature=" + hex.EncodeToString(certHMAC(key, toSign))
	body, err := certDeployPOST(ctx, "https://"+host+"/", "application/json; charset=utf-8", string(payload), map[string]string{"Authorization": auth, "X-TC-Action": "UploadCertificate", "X-TC-Version": "2019-12-05", "X-TC-Timestamp": ts, "X-TC-Region": t.Config["region"]})
	if err != nil {
		return "", err
	}
	var response map[string]json.RawMessage
	if json.Unmarshal(body["Response"], &response) != nil {
		return "", invalid("腾讯云响应格式无效")
	}
	if _, ok := response["Error"]; ok {
		return "", invalid("腾讯云拒绝证书上传，请检查 SSL 权限")
	}
	id := certUploadID(response["CertificateId"])
	if id == "" {
		return "", &AppError{Code: "DEPLOY_UNCERTAIN", Message: "腾讯云未返回证书 ID，请核对证书库"}
	}
	return "已上传到腾讯云 SSL（" + id + "）", nil
}
