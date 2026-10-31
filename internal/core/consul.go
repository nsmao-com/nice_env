package core

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"reflect"
	"strconv"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	"niceenv/internal/process"
)

var consulManagedArgs = []string{"agent", "-server", "-bootstrap-expect", "1", "-node", "niceenv-consul", "-bind", "127.0.0.1", "-client", "127.0.0.1", "-data-dir", "{data}", "-ui", "-http-port", "{port}", "-dns-port", "{port+100}", "-server-port", "{port-200}", "-serf-lan-port", "{port-199}", "-serf-wan-port", "{port-198}", "-grpc-port", "{port+2}", "-grpc-tls-port", "{port+3}", "-hcl", "connect { enabled = true }", "-hcl", "raft_logstore { backend = \"boltdb\" }"}
var consulUDPOffsets = []int{-199, -198, 100}

func validateConsulSpec(p Package) error {
	r := p.Run
	if r == nil || !reflect.DeepEqual(r.Args, consulManagedArgs) || r.SingleInstance != nil && !*r.SingleInstance || r.Health != "tcp" || r.ConfigFile != "" || r.ConfigTemplate != "" || r.DataDir != "" || r.Cwd != "" || len(r.Env) != 0 || len(r.StopArgs) != 0 || len(r.InitArgs) != 0 {
		return &AppError{Code: "CONSUL_CUSTOM_CONFIG", Message: "当前 Consul 不是受管的本机持久化单节点配置，请核对套件运行描述"}
	}
	return nil
}

func (c *Core) registerServiceWeb() {
	c.handle("repair_service_web_ui", c.repairQdrantWeb)
	c.handle("service_web_url", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[serviceRequest](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		e, err := c.serviceEntry(in.ID, "")
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		child, resolved, state := e.child, e.resolved, e.status.State
		c.services.mu.RUnlock()
		if child == nil || resolved == nil || state != "running" {
			return nil, invalid("请先启动服务，再打开管理台")
		}
		if e.pkg.ID == "minio" {
			return minioWebURL(ctx, child, resolved)
		}
		if e.pkg.ID == "qdrant" || e.pkg.ID == "sftpgo" {
			listeners, err := gnet.ConnectionsWithContext(ctx, "tcp")
			if err != nil {
				return nil, err
			}
			if e.pkg.ID == "sftpgo" {
				if resolved.sftpgoWebURL == "" {
					return nil, &AppError{Code: "SERVICE_WEB_UNAVAILABLE", Message: "SFTPGo 管理台未启用本机访问入口，请检查 HTTP 配置"}
				}
				port := resolved.port + 6058
				if port > 65535 {
					return nil, invalid("SFTPGo 管理台端口超出范围")
				}
				for _, listener := range listeners {
					if listener.Status != "LISTEN" || int(listener.Laddr.Port) != port || (listener.Laddr.IP != "127.0.0.1" && listener.Laddr.IP != "0.0.0.0" && listener.Laddr.IP != "::") {
						continue
					}
					owned, err := child.OwnsPID(int(listener.Pid))
					if err != nil {
						return nil, err
					}
					if owned {
						return resolved.sftpgoWebURL, nil
					}
				}
				return nil, &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "SFTPGo 管理台端口未由当前受管服务监听，请刷新状态"}
			}
			for _, listener := range listeners {
				if listener.Status != "LISTEN" || int(listener.Laddr.Port) != resolved.port {
					continue
				}
				owned, err := child.OwnsPID(int(listener.Pid))
				if err != nil {
					return nil, err
				}
				if owned {
					return qdrantWebURL(resolved)
				}
			}
			return nil, &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "Qdrant 管理台端口未由当前服务监听，请刷新状态"}
		}
		if e.pkg.ID != "consul" {
			return genericServiceWebURL(ctx, e.pkg.ID, child, resolved)
		}
		listeners, err := gnet.ConnectionsWithContext(ctx, "tcp4")
		if err != nil {
			return nil, err
		}
		for _, listener := range listeners {
			if listener.Status != "LISTEN" || int(listener.Laddr.Port) != resolved.port || listener.Laddr.IP != "127.0.0.1" {
				continue
			}
			owned, err := child.OwnsPID(int(listener.Pid))
			if err != nil {
				return nil, err
			}
			if owned {
				return fmt.Sprintf("http://127.0.0.1:%d/ui/", resolved.port), nil
			}
		}
		return nil, &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "管理台端口未由当前受管服务监听，请刷新状态"}
	})
}

func checkConsulUDP(ctx context.Context, port int) error {
	listeners := []net.PacketConn{}
	defer func() {
		for _, conn := range listeners {
			conn.Close()
		}
	}()
	for _, offset := range consulUDPOffsets {
		if err := ctx.Err(); err != nil {
			return err
		}
		value := port + offset
		if value < 1 || value > 65535 {
			return invalid("Consul 端口组超出有效范围")
		}
		listener, err := net.ListenPacket("udp4", net.JoinHostPort("127.0.0.1", strconv.Itoa(value)))
		if err != nil {
			return &AppError{Code: "PORT_IN_USE", Message: fmt.Sprintf("Consul UDP 端口 %d 已被占用或无法绑定", value)}
		}
		listeners = append(listeners, listener)
	}
	return nil
}

func consulHTTPClient(timeout time.Duration) (*http.Client, func()) {
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	return &http.Client{Transport: transport, Timeout: timeout, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}, transport.CloseIdleConnections
}

func consulReady(ctx context.Context, port int, child *process.Child) (bool, error) {
	listeners, err := gnet.ConnectionsWithContext(ctx, "udp4")
	if err != nil {
		return false, err
	}
	wanted := map[int]bool{}
	for _, offset := range consulUDPOffsets {
		wanted[port+offset] = true
	}
	for _, listener := range listeners {
		if !wanted[int(listener.Laddr.Port)] {
			continue
		}
		owned, err := child.OwnsPID(int(listener.Pid))
		if err != nil {
			return false, err
		}
		if !owned {
			return false, &AppError{Code: "PORT_OWNED_ELSEWHERE", Message: "Consul UDP 端口属于其它进程"}
		}
		ip := net.ParseIP(listener.Laddr.IP)
		if ip == nil || !ip.IsLoopback() {
			return false, invalid("Consul UDP 端口未限定为本机访问")
		}
		delete(wanted, int(listener.Laddr.Port))
	}
	if len(wanted) != 0 {
		return false, nil
	}
	client, closeClient := consulHTTPClient(700 * time.Millisecond)
	defer closeClient()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("http://127.0.0.1:%d/v1/status/leader", port), nil)
	if err != nil {
		return false, err
	}
	response, err := client.Do(req)
	if err != nil {
		return false, nil
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return false, nil
	}
	var leader string
	if json.NewDecoder(io.LimitReader(response.Body, 4096)).Decode(&leader) != nil {
		return false, nil
	}
	return leader == net.JoinHostPort("127.0.0.1", strconv.Itoa(port-200)), nil
}

func (c *Core) shutdownConsul(ctx context.Context, e *serviceEntry) error {
	c.services.mu.RLock()
	child, resolved := e.child, e.resolved
	c.services.mu.RUnlock()
	if child == nil {
		return nil
	}
	if resolved == nil {
		return invalid("Consul 运行信息缺失，无法验证停机目标")
	}
	listeners, err := gnet.ConnectionsWithContext(ctx, "tcp4")
	if err != nil {
		return err
	}
	owned := false
	for _, listener := range listeners {
		if listener.Status != "LISTEN" || int(listener.Laddr.Port) != resolved.port || listener.Laddr.IP != "127.0.0.1" {
			continue
		}
		match, err := child.OwnsPID(int(listener.Pid))
		if err != nil {
			return err
		}
		if !match {
			return &AppError{Code: "PORT_OWNED_ELSEWHERE", Message: "Consul API 端口属于其它进程，未发送停机请求"}
		}
		owned = true
	}
	if !owned {
		return &AppError{Code: "SERVICE_GRACEFUL_UNAVAILABLE", Message: "Consul API 尚未就绪，无法确认普通停机目标，请检查日志"}
	}
	client, closeClient := consulHTTPClient(45 * time.Second)
	defer closeClient()
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, fmt.Sprintf("http://127.0.0.1:%d/v1/agent/leave", resolved.port), nil)
	if err != nil {
		return err
	}
	response, err := client.Do(req)
	if err != nil {
		select {
		case <-child.Done():
			return nil
		default:
		}
		return &AppError{Code: "CONSUL_STOP_PENDING", Message: "Consul 停机请求未确认完成，请检查日志；未强制结束进程", Detail: err.Error()}
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return &AppError{Code: "CONSUL_STOP_FAILED", Message: fmt.Sprintf("Consul 拒绝停机请求（HTTP %d）", response.StatusCode)}
	}
	return nil
}
