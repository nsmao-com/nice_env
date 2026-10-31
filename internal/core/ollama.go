package core

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"
)

type ollamaModel struct {
	Name         string `json:"name"`
	Digest       string `json:"digest"`
	Size         uint64 `json:"size"`
	Modified     string `json:"modified"`
	Parameters   string `json:"parameters"`
	Quantization string `json:"quantization"`
}
type ollamaPullStatus struct {
	ID        string  `json:"id"`
	Name      string  `json:"name"`
	State     string  `json:"state"`
	Phase     string  `json:"phase"`
	Digest    *string `json:"digest"`
	Completed *uint64 `json:"completed"`
	Total     *uint64 `json:"total"`
	Error     *string `json:"error"`
	StartedAt int64   `json:"startedAt"`
	EndedAt   *int64  `json:"endedAt"`
}
type ollamaManager struct {
	mu      sync.Mutex
	wg      sync.WaitGroup
	closing bool
	status  *ollamaPullStatus
	cancel  context.CancelFunc
}

func (m *ollamaManager) active() bool {
	return m.status != nil && (m.status.State == "pulling" || m.status.State == "cancelling")
}
func (c *Core) closeOllama() {
	c.ollama.mu.Lock()
	c.ollama.closing = true
	if c.ollama.cancel != nil {
		c.ollama.cancel()
	}
	c.ollama.mu.Unlock()
	c.ollama.wg.Wait()
}
func ollamaName(raw string) (string, error) {
	name := strings.TrimSpace(raw)
	if len(name) == 0 || len(name) > 256 || strings.Contains(name, "..") || !regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._/:\-]*$`).MatchString(name) {
		return "", invalid("模型名称无效")
	}
	return name, nil
}
func canonicalModel(name string) string {
	last := name[strings.LastIndex(name, "/")+1:]
	if !strings.Contains(last, ":") {
		return name + ":latest"
	}
	return name
}
func (c *Core) ollamaHTTP(ctx context.Context) (*http.Client, string, *serviceEntry, error) {
	e, err := c.serviceEntry("ollama", "")
	if err != nil {
		return nil, "", nil, err
	}
	if err = c.verifyDatabaseListener(ctx, e); err != nil {
		return nil, "", nil, err
	}
	c.services.mu.RLock()
	port, revision := e.resolved.port, e.revision
	c.services.mu.RUnlock()
	address := net.JoinHostPort("127.0.0.1", strconv.Itoa(port))
	transport := &http.Transport{Proxy: nil, DisableKeepAlives: true, ResponseHeaderTimeout: 15 * time.Second, DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
		if addr != address {
			return nil, invalid("Ollama 请求目标发生变化")
		}
		c.services.mu.RLock()
		same := e.revision == revision && e.child != nil
		c.services.mu.RUnlock()
		if !same {
			return nil, invalid("Ollama 实例已变化")
		}
		if err := c.verifyDatabaseListener(ctx, e); err != nil {
			return nil, err
		}
		return (&net.Dialer{Timeout: 3 * time.Second}).DialContext(ctx, "tcp4", address)
	}}
	client := &http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	return client, "http://" + address, e, nil
}
func ollamaJSON(ctx context.Context, client *http.Client, base, path, method string, body any) ([]byte, error) {
	var input io.Reader
	if body != nil {
		data, err := json.Marshal(body)
		if err != nil {
			return nil, err
		}
		input = bytes.NewReader(data)
	}
	req, err := http.NewRequestWithContext(ctx, method, base+path, input)
	if err != nil {
		return nil, err
	}
	if input != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	response, err := client.Do(req)
	if err != nil {
		return nil, &AppError{Code: "OLLAMA_REQUEST_FAILED", Message: "无法完成 Ollama 请求，请检查受管服务"}
	}
	defer response.Body.Close()
	data, err := io.ReadAll(io.LimitReader(response.Body, 4*1024*1024+1))
	if err != nil {
		return nil, err
	}
	if len(data) > 4*1024*1024 {
		return nil, invalid("Ollama 响应超过限制")
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return nil, &AppError{Code: "OLLAMA_REQUEST_FAILED", Message: "Ollama 返回错误，请检查模型名称及服务日志", Detail: strconv.Itoa(response.StatusCode)}
	}
	return data, nil
}
func ollamaModels(ctx context.Context, client *http.Client, base string) ([]ollamaModel, error) {
	data, err := ollamaJSON(ctx, client, base, "/api/tags", "GET", nil)
	if err != nil {
		return nil, err
	}
	var wire struct {
		Models []struct {
			Name     string `json:"name"`
			Digest   string `json:"digest"`
			Size     uint64 `json:"size"`
			Modified string `json:"modified_at"`
			Details  struct {
				Parameters   string `json:"parameter_size"`
				Quantization string `json:"quantization_level"`
			} `json:"details"`
		} `json:"models"`
	}
	if err = json.Unmarshal(data, &wire); err != nil {
		return nil, invalid("Ollama 模型列表格式无效")
	}
	out := []ollamaModel{}
	for _, model := range wire.Models {
		out = append(out, ollamaModel{model.Name, model.Digest, model.Size, model.Modified, model.Details.Parameters, model.Details.Quantization})
	}
	return out, nil
}
func (c *Core) registerOllama() {
	c.handle("ollama_models", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.ollamaCommand(ctx, data, "models")
	})
	c.handle("ollama_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.ollamaCommand(ctx, data, "delete")
	})
	c.handle("ollama_pull", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.ollamaCommand(ctx, data, "pull")
	})
	c.handle("ollama_pull_status", func(context.Context, json.RawMessage) (any, error) {
		c.ollama.mu.Lock()
		defer c.ollama.mu.Unlock()
		if c.ollama.status == nil {
			return nil, nil
		}
		return *c.ollama.status, nil
	})
	c.handle("ollama_cancel_pull", func(_ context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		c.ollama.mu.Lock()
		defer c.ollama.mu.Unlock()
		if c.ollama.status == nil || c.ollama.status.ID != in.ID {
			return nil, invalid("拉取任务已变化，请刷新")
		}
		if c.ollama.active() {
			c.ollama.status.State = "cancelling"
			c.ollama.cancel()
		}
		return true, nil
	})
}
func (c *Core) ollamaCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Name string `json:"name"`
	}](data)
	if err != nil {
		return nil, err
	}
	name := ""
	if action != "models" {
		name, err = ollamaName(in.Name)
		if err != nil {
			return nil, err
		}
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	client, base, e, err := c.ollamaHTTP(ctx)
	if err != nil {
		return nil, err
	}
	if action == "models" {
		return ollamaModels(ctx, client, base)
	}
	c.ollama.mu.Lock()
	defer c.ollama.mu.Unlock()
	if c.ollama.closing {
		return nil, invalid("应用正在退出")
	}
	if c.ollama.active() {
		return nil, invalid("请先等待当前拉取结束或取消")
	}
	if action == "delete" {
		models, err := ollamaModels(ctx, client, base)
		if err != nil {
			return nil, err
		}
		found := false
		for _, model := range models {
			found = found || canonicalModel(model.Name) == canonicalModel(name)
		}
		if !found {
			return nil, invalid("模型已不存在，请刷新列表")
		}
		if _, err = ollamaJSON(ctx, client, base, "/api/delete", "DELETE", map[string]string{"model": name}); err != nil {
			return nil, err
		}
		models, err = ollamaModels(ctx, client, base)
		if err != nil {
			return nil, err
		}
		for _, model := range models {
			if canonicalModel(model.Name) == canonicalModel(name) {
				return nil, invalid("Ollama 仍返回此模型，无法确认删除")
			}
		}
		return true, nil
	}
	id, err := newID("ollama")
	if err != nil {
		return nil, err
	}
	workCtx, workCancel := context.WithTimeout(context.Background(), 2*time.Hour)
	c.ollama.cancel = workCancel
	status := ollamaPullStatus{ID: id, Name: name, State: "pulling", Phase: "pulling manifest", StartedAt: time.Now().UnixMilli()}
	c.ollama.status = &status
	c.ollama.wg.Add(1)
	c.services.mu.RLock()
	done := e.done
	c.services.mu.RUnlock()
	go func() {
		defer c.ollama.wg.Done()
		defer workCancel()
		finished := make(chan struct{})
		go func() {
			select {
			case <-done:
				workCancel()
			case <-c.closed:
				workCancel()
			case <-finished:
			}
		}()
		err := c.pullOllama(workCtx, client, base, id, name)
		close(finished)
		c.ollama.mu.Lock()
		defer c.ollama.mu.Unlock()
		if c.ollama.status.ID != id {
			return
		}
		status := c.ollama.status
		ended := time.Now().UnixMilli()
		status.EndedAt = &ended
		if status.State == "cancelling" {
			status.State = "cancelled"
		} else if err != nil {
			status.State = "failed"
			message := publicError(err).Message
			status.Error = &message
		} else {
			status.State = "succeeded"
			status.Phase = "success"
		}
	}()
	return status, nil
}
func (c *Core) pullOllama(ctx context.Context, client *http.Client, base, id, name string) error {
	data, _ := json.Marshal(map[string]any{"model": name, "stream": true})
	req, err := http.NewRequestWithContext(ctx, "POST", base+"/api/pull", bytes.NewReader(data))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	workCtx, cancel := context.WithCancel(req.Context())
	defer cancel()
	req = req.WithContext(workCtx)
	timer := time.AfterFunc(120*time.Second, cancel)
	defer timer.Stop()
	response, err := client.Do(req)
	if err != nil {
		return &AppError{Code: "OLLAMA_PULL_FAILED", Message: "模型拉取请求失败或服务已停止"}
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return invalid("Ollama 拒绝拉取，请检查模型名称与服务日志")
	}
	scanner := bufio.NewScanner(response.Body)
	scanner.Buffer(make([]byte, 4096), 65536)
	success := false
	for scanner.Scan() {
		timer.Reset(120 * time.Second)
		if len(bytes.TrimSpace(scanner.Bytes())) == 0 {
			continue
		}
		var progress struct {
			Status    string  `json:"status"`
			Error     string  `json:"error"`
			Digest    *string `json:"digest"`
			Completed *uint64 `json:"completed"`
			Total     *uint64 `json:"total"`
		}
		if err = json.Unmarshal(scanner.Bytes(), &progress); err != nil {
			return invalid("Ollama 拉取进度格式无效")
		}
		if progress.Error != "" {
			return &AppError{Code: "OLLAMA_PULL_FAILED", Message: "模型拉取失败", Detail: redactDiagnostic(progress.Error)}
		}
		if progress.Status == "" || len(progress.Status) > 256 {
			return invalid("Ollama 拉取状态无效")
		}
		if progress.Digest != nil && len(*progress.Digest) > 256 {
			return invalid("Ollama 拉取摘要过长")
		}
		c.ollama.mu.Lock()
		if c.ollama.status.ID == id {
			c.ollama.status.Phase = progress.Status
			c.ollama.status.Digest = progress.Digest
			c.ollama.status.Completed = progress.Completed
			c.ollama.status.Total = progress.Total
		}
		c.ollama.mu.Unlock()
		success = progress.Status == "success"
	}
	if err = scanner.Err(); err != nil {
		return &AppError{Code: "OLLAMA_STREAM_FAILED", Message: "模型拉取连接中断或等待超时"}
	}
	if !success {
		return invalid("拉取连接结束但未返回成功状态")
	}
	models, err := ollamaModels(ctx, client, base)
	if err != nil {
		return err
	}
	for _, model := range models {
		if canonicalModel(model.Name) == canonicalModel(name) {
			return nil
		}
	}
	return invalid("模型尚未出现在 Ollama 列表中，未确认拉取成功")
}
