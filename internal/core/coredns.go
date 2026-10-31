package core

import (
	"context"
	"fmt"
	"net"
	"path/filepath"
	"reflect"
	"regexp"
	"strconv"
	"strings"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	"niceenv/internal/process"
)

var dnsZonePattern = regexp.MustCompile(`^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*$`)

func (c *Core) prepareCoreDNS(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	run := e.pkg.Run
	if !reflect.DeepEqual(run.Args, []string{"-conf", "{etc}/Corefile", "-dns.port", "{port}"}) || run.ConfigFile != "Corefile" || run.Health != "tcp" || len(run.InitArgs) != 0 || len(run.StopArgs) != 0 || len(run.Env) != 0 || run.SingleInstance != nil && !*run.SingleInstance {
		return invalid("CoreDNS 运行描述必须使用受管 Corefile 和本机监听端口")
	}
	zone, _, err := c.Store.setting(ctx, "defaultTld")
	if err != nil {
		return err
	}
	if zone == "" {
		zone = "test"
	}
	zone = strings.ToLower(strings.Trim(strings.TrimSpace(zone), "."))
	if len(zone) > 253 || net.ParseIP(zone) != nil || !dnsZonePattern.MatchString(zone) {
		return invalid("本地域名后缀无效，请填写 test 或 dev.example 等域名后缀")
	}
	r.dnsZone = zone
	if err := checkServicePorts(ctx, r.ports); err != nil {
		return err
	}
	// CoreDNS uses both protocols. Refuse an occupied UDP socket before writing
	// configuration or starting a process; the generic guard checks TCP as well.
	listener, err := net.ListenPacket("udp4", net.JoinHostPort("127.0.0.1", strconv.Itoa(r.port)))
	if err != nil {
		return &AppError{Code: "PORT_IN_USE", Message: fmt.Sprintf("CoreDNS UDP 端口 %d 已被占用或无法绑定", r.port)}
	}
	listener.Close()
	text := fmt.Sprintf(`# NiceEnv managed Corefile — *.%s resolves to 127.0.0.1
%s {
    bind 127.0.0.1
    template IN A %s {
        answer "{{ .Name }} 60 IN A 127.0.0.1"
    }
    template IN ANY %s {
        rcode NOERROR
    }
    errors
}
. {
    bind 127.0.0.1
    forward . 8.8.8.8 1.1.1.1
    errors
}
`, zone, zone, zone, zone)
	// As in the legacy adapter this file follows the selected TLD at startup.
	// Existing content is backed up by the shared atomic configuration writer.
	target := configTarget{key: "coredns-corefile@" + e.pkg.Version, path: filepath.Join(r.etc, "Corefile")}
	return c.writeConfig(ctx, target, text, nil)
}

func coreDNSReady(ctx context.Context, r *resolvedService, child *process.Child) (bool, error) {
	listeners, err := gnet.ConnectionsWithContext(ctx, "udp4")
	if err != nil {
		return false, err
	}
	owned := false
	for _, l := range listeners {
		if int(l.Laddr.Port) != r.port {
			continue
		}
		match, err := child.OwnsPID(int(l.Pid))
		if err != nil {
			return false, err
		}
		if !match || l.Laddr.IP != "127.0.0.1" {
			return false, &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "CoreDNS UDP 端口未由本次受管进程限定在本机监听"}
		}
		owned = true
	}
	if !owned {
		return false, nil
	}
	probeCtx, cancel := context.WithTimeout(ctx, 700*time.Millisecond)
	defer cancel()
	resolver := net.Resolver{PreferGo: true, StrictErrors: true, Dial: func(ctx context.Context, network, address string) (net.Conn, error) {
		return (&net.Dialer{Timeout: 350 * time.Millisecond}).DialContext(ctx, network, net.JoinHostPort("127.0.0.1", strconv.Itoa(r.port)))
	}}
	id, err := newID("niceenv-check")
	if err != nil {
		return false, err
	}
	ips, err := resolver.LookupIP(probeCtx, "ip4", id+"."+r.dnsZone+".")
	if err != nil {
		return false, nil
	}
	return len(ips) == 1 && ips[0].Equal(net.IPv4(127, 0, 0, 1)), nil
}
