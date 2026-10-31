package core

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	gprocess "github.com/shirou/gopsutil/v4/process"
	"niceenv/internal/process"
)

type portListener struct {
	Port               int    `json:"port"`
	PID                int    `json:"pid"`
	ProcessName        string `json:"processName,omitempty"`
	Cmdline            string `json:"cmdline,omitempty"`
	OwnedBySelf        bool   `json:"ownedBySelf"`
	ServiceID          string `json:"serviceId,omitempty"`
	ProcessStartedAt   int64  `json:"processStartedAt,omitempty"`
	ProcessStartMarker string `json:"processStartMarker,omitempty"`
	Ownership          string `json:"ownership"`
	CanClose           bool   `json:"canClose"`
	CloseReason        string `json:"closeReason,omitempty"`
}

func validPortRange(from, to int) error {
	if from < 1 || to > 65535 || from > to || to-from > 4095 {
		return invalid("端口范围须位于 1–65535 且单次不超过 4096 个")
	}
	return nil
}
func (c *Core) portListeners(ctx context.Context, from, to int) ([]portListener, error) {
	connections, err := gnet.ConnectionsWithContext(ctx, "tcp")
	if err != nil {
		return nil, err
	}
	protected := map[int]bool{0: true, 1: true, 4: true, os.Getpid(): true}
	parent := int32(os.Getpid())
	for i := 0; i < 128; i++ {
		p, err := gprocess.NewProcess(parent)
		if err != nil {
			break
		}
		parent, err = p.PpidWithContext(ctx)
		if err != nil || parent <= 0 || protected[int(parent)] {
			break
		}
		protected[int(parent)] = true
	}
	c.services.mu.RLock()
	entries := make([]*serviceEntry, 0, len(c.services.entries))
	for _, e := range c.services.entries {
		entries = append(entries, e)
	}
	c.services.mu.RUnlock()
	out := []portListener{}
	seen := map[string]bool{}
	for _, conn := range connections {
		port := int(conn.Laddr.Port)
		if conn.Status != "LISTEN" || port < from || port > to {
			continue
		}
		key := fmt.Sprintf("%d/%d", port, conn.Pid)
		if seen[key] {
			continue
		}
		seen[key] = true
		item := portListener{Port: port, PID: int(conn.Pid), Ownership: "unknown"}
		unknown := false
		for _, e := range entries {
			c.services.mu.RLock()
			child, id := e.child, e.status.ID
			c.services.mu.RUnlock()
			if child == nil {
				continue
			}
			owned, err := child.OwnsPID(item.PID)
			if err != nil {
				unknown = true
				continue
			}
			if owned {
				item.OwnedBySelf = true
				item.ServiceID = id
				item.Ownership = "self"
				break
			}
		}
		p, err := gprocess.NewProcess(conn.Pid)
		if err == nil {
			item.ProcessName, _ = p.NameWithContext(ctx)
			cmd, _ := p.CmdlineWithContext(ctx)
			item.Cmdline = redactDiagnostic(cmd)
			created, _ := p.CreateTimeWithContext(ctx)
			item.ProcessStartedAt = created / 1000
			if !item.OwnedBySelf && !unknown {
				item.Ownership = "external"
			}
		}
		item.ProcessStartMarker, _ = process.Identity(item.PID)
		if protected[item.PID] || item.PID <= 4 {
			item.CloseReason = "系统进程或 NiceEnv 所在进程链不能从此处结束"
		} else if item.ProcessStartMarker == "" {
			item.CloseReason = "无法读取准确进程身份，请重新扫描"
		} else if item.Ownership == "unknown" {
			item.CloseReason = "无法确定进程归属，请重新扫描"
		} else {
			item.CanClose = true
		}
		out = append(out, item)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Port != out[j].Port {
			return out[i].Port < out[j].Port
		}
		return out[i].PID < out[j].PID
	})
	return out, nil
}
func (c *Core) registerPorts() {
	c.handle("scan_port_range", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			From int `json:"from"`
			To   int `json:"to"`
		}](data)
		if err != nil {
			return nil, err
		}
		if err = validPortRange(in.From, in.To); err != nil {
			return nil, err
		}
		ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
		defer cancel()
		listeners, err := c.portListeners(ctx, in.From, in.To)
		return map[string]any{"from": in.From, "to": in.To, "scannedAt": time.Now().UnixMilli(), "listeners": listeners}, err
	})
	c.handle("scan_ports", c.scanPorts)
	c.handle("close_port", c.closePort)
	c.handle("set_port_override", c.setPortOverride)
}
func (c *Core) scanPorts(ctx context.Context, _ json.RawMessage) (any, error) {
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	listeners, err := c.portListeners(ctx, 1, 65535)
	if err != nil {
		return nil, err
	}
	out := []map[string]any{}
	c.services.mu.RLock()
	defer c.services.mu.RUnlock()
	for _, e := range c.services.entries {
		ports := []int{e.status.Port}
		if e.child != nil && e.resolved != nil {
			ports = e.resolved.ports
		}
		for _, port := range ports {
			if port < 1 {
				continue
			}
			matches := []portListener{}
			for _, l := range listeners {
				if l.Port == port {
					matches = append(matches, l)
				}
			}
			verdict, detail := "free", "TCP 端口当前未监听"
			running := e.child != nil
			if len(matches) == 0 && running {
				verdict, detail = "missing", "受管进程存在但端口未监听"
			}
			self := len(matches) > 0
			for _, l := range matches {
				self = self && l.ServiceID == e.status.ID
			}
			if len(matches) > 0 {
				if self {
					verdict, detail = "self", "监听者属于本服务"
				} else {
					verdict, detail = "conflict", "监听者不完全属于本服务"
					for _, l := range matches {
						if l.Ownership == "unknown" {
							verdict, detail = "unknown", "无法确认监听进程归属"
						}
					}
				}
			}
			row := map[string]any{"serviceId": e.status.ID, "label": e.status.Label, "port": port, "ownedBySelf": self, "running": running, "verdict": verdict, "detail": detail, "listenerCount": len(matches)}
			if len(matches) > 0 {
				row["pid"] = matches[0].PID
				row["processName"] = matches[0].ProcessName
				row["cmdline"] = matches[0].Cmdline
			}
			out = append(out, row)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i]["port"].(int) < out[j]["port"].(int) })
	return out, nil
}
func (c *Core) closePort(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Port     int            `json:"port"`
		Expected []portListener `json:"expected"`
	}](data)
	if err != nil {
		return nil, err
	}
	if err = validPortRange(in.Port, in.Port); err != nil {
		return nil, err
	}
	if len(in.Expected) == 0 || len(in.Expected) > 128 {
		return nil, invalid("请先扫描并确认监听进程")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	current, err := c.portListeners(ctx, in.Port, in.Port)
	if err != nil {
		return nil, err
	}
	same := func(a, b []portListener) bool {
		if len(a) != len(b) {
			return false
		}
		for _, x := range a {
			if !slices.ContainsFunc(b, func(y portListener) bool {
				return x.Port == y.Port && x.PID == y.PID && x.ProcessStartMarker != "" && x.ProcessStartMarker == y.ProcessStartMarker && x.ServiceID == y.ServiceID
			}) {
				return false
			}
		}
		return true
	}
	if !same(current, in.Expected) {
		return nil, invalid("端口监听者已变化，请重新扫描后确认")
	}
	for _, l := range current {
		if !l.CanClose {
			return nil, invalid(l.CloseReason)
		}
	}
	killed := []int{}
	messages := []string{}
	stopped := map[string]bool{}
	graceful := false
	serviceID := ""
	for _, l := range current {
		if l.OwnedBySelf {
			if stopped[l.ServiceID] {
				continue
			}
			stopped[l.ServiceID] = true
			graceful = true
			serviceID = l.ServiceID
			e, err := c.serviceEntry(l.ServiceID, "")
			if err == nil {
				err = c.stopOwned(ctx, e, false)
			}
			if err != nil {
				messages = append(messages, publicError(err).Message)
			}
			continue
		}
		latest, err := c.portListeners(ctx, in.Port, in.Port)
		if err != nil {
			messages = append(messages, "无法复核端口监听者")
			break
		}
		if !slices.ContainsFunc(latest, func(x portListener) bool {
			return x.PID == l.PID && x.ProcessStartMarker == l.ProcessStartMarker && x.CanClose && !x.OwnedBySelf
		}) {
			messages = append(messages, "进程身份或归属变化，未结束该进程")
			continue
		}
		if err = process.TerminateIdentity(l.PID, l.ProcessStartMarker); err != nil {
			messages = append(messages, "结束进程失败："+strconv.Itoa(l.PID))
		} else {
			killed = append(killed, l.PID)
		}
	}
	remaining, err := c.portListeners(ctx, in.Port, in.Port)
	if err != nil {
		return nil, err
	}
	return map[string]any{"port": in.Port, "graceful": graceful, "serviceId": serviceID, "killedPids": killed, "portFree": len(remaining) == 0, "remaining": remaining, "errors": messages}, nil
}
func (c *Core) setPortOverride(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Key  string `json:"key"`
		Port *int   `json:"port"`
	}](data)
	if err != nil {
		return nil, err
	}
	if in.Key == "" || len(in.Key) > 150 || strings.ContainsAny(in.Key, "/\\\x00") {
		return nil, invalid("端口键名无效")
	}
	if in.Port != nil && (*in.Port < 1 || *in.Port > 65535) {
		return nil, invalid("端口须为 1–65535")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	pkg := map[string]string{"http": "nginx", "https": "nginx", "apacheHttp": "apache", "apacheHttps": "apache", "postgres": "postgresql", "caddyHttps": "caddy"}[in.Key]
	if pkg == "" {
		pkg = in.Key
	}
	c.services.mu.RLock()
	known := slices.Contains([]string{"http", "https", "mysql", "redis", "postgres", "mongodb", "apacheHttp", "apacheHttps", "caddy", "caddyHttps"}, in.Key)
	running := false
	for _, e := range c.services.entries {
		if e.pkg.ID == pkg || e.status.ID == in.Key {
			known = true
			running = running || e.child != nil
		}
	}
	c.services.mu.RUnlock()
	if !known {
		return nil, invalid("不支持的服务端口设置")
	}
	if running {
		return nil, invalid("请先停止服务再更改端口")
	}
	if strings.HasPrefix(in.Key, "php@") && in.Port != nil && *in.Port > 65532 {
		return nil, invalid("PHP 池需要连续 4 个端口")
	}
	if in.Port == nil {
		_, err = c.Store.db.ExecContext(ctx, "DELETE FROM settings WHERE key=?", "portOverride."+in.Key)
	} else {
		err = c.Store.saveSetting(ctx, "portOverride."+in.Key, *in.Port)
	}
	if err != nil {
		return nil, err
	}
	return true, c.refreshServices(ctx)
}
