package core

import (
	"bufio"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	"niceenv/internal/process"
)

type controlEndpoint struct {
	Address string `json:"address"`
	Token   string `json:"token"`
	Version string `json:"version"`
}
type controlManager struct {
	listener net.Listener
	unlock   func() error
	wg       sync.WaitGroup
	slots    chan struct{}
}
type controlRequest struct {
	Command string          `json:"command"`
	Args    json.RawMessage `json:"args"`
}
type controlEnvelope struct {
	Body json.RawMessage `json:"body"`
	MAC  string          `json:"mac"`
}

func controlMAC(token, nonce string, body []byte) string {
	h := hmac.New(sha256.New, []byte(token))
	h.Write([]byte(nonce))
	h.Write([]byte{0})
	h.Write(body)
	return hex.EncodeToString(h.Sum(nil))
}
func readControlLine(reader *bufio.Reader, max int) ([]byte, error) {
	var result []byte
	for {
		line, prefix, err := reader.ReadLine()
		if err != nil {
			return nil, err
		}
		if len(result)+len(line) > max {
			return nil, invalid("本地控制消息超过大小限制")
		}
		result = append(result, line...)
		if !prefix {
			return result, nil
		}
	}
}
func (c *Core) startControl() error {
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return err
	}
	token, err := newID("control")
	if err != nil {
		listener.Close()
		return err
	}
	endpoint := controlEndpoint{Address: listener.Addr().String(), Token: token, Version: c.Version}
	body, err := json.Marshal(endpoint)
	if err != nil {
		listener.Close()
		return err
	}
	path := filepath.Join(c.Home, ".niceenv-wails-control.json")
	f, err := os.CreateTemp(c.Home, ".control-")
	if err != nil {
		listener.Close()
		return err
	}
	defer os.Remove(f.Name())
	if err = process.PrivateFile(f.Name()); err == nil {
		_, err = f.Write(body)
	}
	err = errors.Join(err, f.Sync(), f.Close())
	if err == nil {
		err = os.Rename(f.Name(), path)
	}
	if err != nil {
		listener.Close()
		return err
	}
	c.control.listener = listener
	c.control.slots = make(chan struct{}, 16)
	c.control.wg.Add(1)
	go func() {
		defer c.control.wg.Done()
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			select {
			case c.control.slots <- struct{}{}:
				c.control.wg.Add(1)
				go func() {
					defer c.control.wg.Done()
					defer func() { <-c.control.slots }()
					c.serveControl(conn, endpoint)
				}()
			default:
				conn.Close()
			}
		}
	}()
	return nil
}
func (c *Core) serveControl(conn net.Conn, endpoint controlEndpoint) {
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
	nonce, err := newID("request")
	if err != nil {
		return
	}
	if err = json.NewEncoder(conn).Encode(map[string]string{"nonce": nonce}); err != nil {
		return
	}
	line, err := readControlLine(bufio.NewReader(conn), 6*1024*1024)
	if err != nil {
		return
	}
	var envelope controlEnvelope
	if json.Unmarshal(line, &envelope) != nil || !hmac.Equal([]byte(envelope.MAC), []byte(controlMAC(endpoint.Token, nonce, envelope.Body))) {
		return
	}
	var request controlRequest
	if json.Unmarshal(envelope.Body, &request) != nil {
		return
	}
	_ = conn.SetDeadline(time.Now().Add(30 * time.Minute))
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Minute)
	defer cancel()
	finished := make(chan struct{})
	defer close(finished)
	go func() {
		select {
		case <-c.closed:
			cancel()
			_ = conn.Close()
		case <-finished:
		}
	}()
	result, callErr := c.Invoke(ctx, request.Command, request.Args)
	response := map[string]any{"result": result}
	if callErr != nil {
		response = map[string]any{"error": json.RawMessage(MarshalError(callErr))}
	}
	body, err := json.Marshal(response)
	if err != nil || len(body) > 16*1024*1024 {
		return
	}
	_ = json.NewEncoder(conn).Encode(controlEnvelope{Body: body, MAC: controlMAC(endpoint.Token, nonce+":reply", body)})
}

// ControlInvoke only connects to a live owner. It never opens another Core or
// retries an operation whose result was lost after sending the request.
func ControlInvoke(ctx context.Context, home, command string, args json.RawMessage) (json.RawMessage, error) {
	path := filepath.Join(home, ".niceenv-wails-control.json")
	file, err := os.Open(path)
	if err != nil {
		return nil, &AppError{Code: "CONTROLLER_UNAVAILABLE", Message: "请先启动同一数据目录的 NiceEnv 桌面应用"}
	}
	data, err := io.ReadAll(io.LimitReader(file, 16385))
	file.Close()
	if err != nil {
		return nil, err
	}
	if len(data) > 16384 {
		return nil, invalid("本地控制通道描述无效")
	}
	var endpoint controlEndpoint
	if json.Unmarshal(data, &endpoint) != nil {
		return nil, invalid("本地控制通道描述无效")
	}
	host, port, err := net.SplitHostPort(endpoint.Address)
	number, _ := strconv.Atoi(port)
	if err != nil || host != "127.0.0.1" || number < 1 || number > 65535 || len(endpoint.Token) < 32 {
		return nil, invalid("本地控制通道地址无效")
	}
	conn, err := (&net.Dialer{Timeout: 3 * time.Second}).DialContext(ctx, "tcp4", endpoint.Address)
	if err != nil {
		return nil, &AppError{Code: "CONTROLLER_UNAVAILABLE", Message: "无法连接当前 NiceEnv，请重新打开应用"}
	}
	defer conn.Close()
	deadline := time.Now().Add(30 * time.Minute)
	if d, ok := ctx.Deadline(); ok {
		deadline = d
	}
	_ = conn.SetDeadline(deadline)
	stop := context.AfterFunc(ctx, func() { conn.Close() })
	defer stop()
	reader := bufio.NewReader(conn)
	line, err := readControlLine(reader, 4096)
	if err != nil {
		return nil, err
	}
	var challenge struct {
		Nonce string `json:"nonce"`
	}
	if json.Unmarshal(line, &challenge) != nil || len(challenge.Nonce) < 32 || len(challenge.Nonce) > 128 {
		return nil, invalid("本地控制握手无效")
	}
	if len(args) == 0 {
		args = json.RawMessage(`{}`)
	}
	body, err := json.Marshal(controlRequest{Command: command, Args: args})
	if err != nil {
		return nil, err
	}
	if len(body) > 4*1024*1024 {
		return nil, invalid("本地控制请求过大")
	}
	if err = json.NewEncoder(conn).Encode(controlEnvelope{Body: body, MAC: controlMAC(endpoint.Token, challenge.Nonce, body)}); err != nil {
		return nil, err
	}
	line, err = readControlLine(reader, 20*1024*1024)
	if err != nil {
		return nil, &AppError{Code: "CONTROL_RESPONSE_LOST", Message: "控制请求已发送，但未取得结果；请先刷新状态，避免重复执行", Detail: err.Error()}
	}
	var response controlEnvelope
	if json.Unmarshal(line, &response) != nil || !hmac.Equal([]byte(response.MAC), []byte(controlMAC(endpoint.Token, challenge.Nonce+":reply", response.Body))) {
		return nil, invalid("本地控制响应身份校验失败")
	}
	var result struct {
		Result json.RawMessage `json:"result"`
		Error  *AppError       `json:"error"`
	}
	if err = json.Unmarshal(response.Body, &result); err != nil {
		return nil, err
	}
	if result.Error != nil {
		return nil, result.Error
	}
	return result.Result, nil
}
