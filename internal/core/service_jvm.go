package core

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/xml"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"

	"niceenv/internal/process"
)

func (c *Core) serviceRuntime(ctx context.Context, id string) (string, string, error) {
	_, exe, err := c.installedTool(ctx, id)
	if err != nil {
		return "", "", err
	}
	return filepath.Dir(filepath.Dir(exe)), exe, nil
}
func managedCopyDefaults(home, source, dest string) error {
	return filepath.WalkDir(source, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.Type()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return invalid("服务默认配置不能包含符号链接")
		}
		rel, err := filepath.Rel(source, path)
		if err != nil {
			return err
		}
		target := filepath.Join(dest, rel)
		if entry.IsDir() {
			return managedDirectory(home, target)
		}
		if info, err := os.Lstat(target); err == nil {
			if !info.Mode().IsRegular() {
				return invalid("服务配置不是普通文件")
			}
			return nil
		} else if !os.IsNotExist(err) {
			return err
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() || info.Size() > 4*1024*1024 {
			return invalid("服务默认配置文件无效或过大")
		}
		body, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		return atomicWrite(target, body)
	})
}
func managedConfigValue(text, key, value string) string {
	lines := strings.Split(strings.ReplaceAll(text, "\r\n", "\n"), "\n")
	out := make([]string, 0, len(lines)+1)
	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		left, _, found := strings.Cut(trimmed, "=")
		if found && strings.TrimSpace(left) == key {
			continue
		}
		out = append(out, line)
	}
	return strings.TrimRight(strings.Join(out, "\n"), "\n") + "\n" + key + "=" + value + "\n"
}

func (c *Core) prepareTomcat(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	home := filepath.Dir(filepath.Dir(r.program))
	javaHome, java, err := c.serviceRuntime(ctx, "temurin-jdk21")
	if err != nil {
		return err
	}
	for _, name := range []string{"conf", "logs", "webapps", "work", "temp", "lib"} {
		if err = managedDirectory(c.Home, filepath.Join(r.data, name)); err != nil {
			return err
		}
	}
	if err = managedCopyDefaults(c.Home, filepath.Join(home, "conf"), filepath.Join(r.data, "conf")); err != nil {
		return err
	}
	path := filepath.Join(r.data, "conf", "server.xml")
	text, err := readConfigText(path)
	if err != nil {
		return err
	}
	decoder := xml.NewDecoder(strings.NewReader(text))
	var output bytes.Buffer
	encoder := xml.NewEncoder(&output)
	httpCount := 0
	for {
		token, err := decoder.Token()
		if err == io.EOF {
			break
		}
		if err != nil {
			return invalid("Tomcat server.xml 格式无效")
		}
		if start, ok := token.(xml.StartElement); ok {
			set := func(key, value string) {
				for i := range start.Attr {
					if start.Attr[i].Name.Local == key {
						start.Attr[i].Value = value
						return
					}
				}
				start.Attr = append(start.Attr, xml.Attr{Name: xml.Name{Local: key}, Value: value})
			}
			if start.Name.Local == "Server" {
				set("port", "-1")
			}
			if start.Name.Local == "Connector" {
				protocol := "HTTP/1.1"
				for _, attr := range start.Attr {
					if attr.Name.Local == "protocol" {
						protocol = attr.Value
					}
				}
				if strings.Contains(strings.ToLower(protocol), "http") {
					httpCount++
					if httpCount > 1 {
						return invalid("Tomcat 存在多个 HTTP Connector，请先明确主监听配置")
					}
					set("port", strconv.Itoa(r.port))
					set("address", "127.0.0.1")
				}
			}
			token = start
		}
		if err = encoder.EncodeToken(token); err != nil {
			return err
		}
	}
	if httpCount != 1 {
		return invalid("Tomcat 未找到 HTTP Connector")
	}
	if err = encoder.Flush(); err != nil {
		return err
	}
	if err = c.writeConfig(ctx, configTarget{key: "tomcat-server", path: path}, output.String(), &text); err != nil {
		return err
	}
	pathText := func(path string) string { return filepath.ToSlash(DisplayPath(path)) }
	r.program, r.cwd = java, r.data
	r.stopArgs = nil
	r.args = []string{"--add-opens=java.base/java.lang=ALL-UNNAMED", "--add-opens=java.base/java.io=ALL-UNNAMED", "--add-opens=java.base/java.util=ALL-UNNAMED", "--add-opens=java.base/java.util.concurrent=ALL-UNNAMED", "--add-opens=java.rmi/sun.rmi.transport=ALL-UNNAMED", "-Dcatalina.home=" + pathText(home), "-Dcatalina.base=" + pathText(r.data), "-Djava.io.tmpdir=" + pathText(filepath.Join(r.data, "temp")), "-Djava.util.logging.manager=org.apache.juli.ClassLoaderLogManager", "-Djava.util.logging.config.file=" + pathText(filepath.Join(r.data, "conf", "logging.properties")), "-classpath", filepath.Join(home, "bin", "bootstrap.jar") + string(os.PathListSeparator) + filepath.Join(home, "bin", "tomcat-juli.jar"), "org.apache.catalina.startup.Bootstrap", "start"}
	r.env = map[string]string{"JAVA_HOME": DisplayPath(javaHome), "CATALINA_HOME": DisplayPath(home), "CATALINA_BASE": DisplayPath(r.data)}
	r.removeEnvironment = []string{"JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH"}
	r.ports = []int{r.port}
	return nil
}

func (c *Core) prepareNeo4j(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	home := filepath.Dir(filepath.Dir(r.program))
	javaHome, _, err := c.serviceRuntime(ctx, "temurin-jdk21")
	if err != nil {
		return err
	}
	if err = managedCopyDefaults(c.Home, filepath.Join(home, "conf"), r.etc); err != nil {
		return err
	}
	for _, name := range []string{"data", "logs", "run", "import", "plugins", "transactions"} {
		if err = managedDirectory(c.Home, filepath.Join(r.data, name)); err != nil {
			return err
		}
	}
	path := filepath.Join(r.etc, "neo4j.conf")
	text, err := readConfigText(path)
	if err != nil {
		return err
	}
	original := text
	bolt := r.port + 213
	if bolt > 65535 {
		return invalid("Neo4j Bolt 端口超出范围")
	}
	settings := [][2]string{{"server.default_listen_address", "127.0.0.1"}, {"server.default_advertised_address", "localhost"}, {"server.http.listen_address", fmt.Sprintf("127.0.0.1:%d", r.port)}, {"server.http.advertised_address", fmt.Sprintf("localhost:%d", r.port)}, {"server.bolt.listen_address", fmt.Sprintf("127.0.0.1:%d", bolt)}, {"server.bolt.advertised_address", fmt.Sprintf("localhost:%d", bolt)}, {"server.https.enabled", "false"}, {"server.memory.heap.initial_size", "256m"}, {"server.memory.heap.max_size", "512m"}, {"server.memory.pagecache.size", "128m"}}
	for _, name := range []string{"data", "logs", "run", "import", "plugins"} {
		settings = append(settings, [2]string{"server.directories." + name, filepath.ToSlash(DisplayPath(filepath.Join(r.data, name)))})
	}
	settings = append(settings, [2]string{"server.directories.transaction.logs.root", filepath.ToSlash(DisplayPath(filepath.Join(r.data, "transactions")))})
	for _, setting := range settings {
		text = managedConfigValue(text, setting[0], setting[1])
	}
	if err = c.writeConfig(ctx, configTarget{key: "neo4j-config", path: path}, text, &original); err != nil {
		return err
	}
	r.cwd = home
	r.args = []string{"console"}
	r.stopArgs = nil
	r.ports = []int{r.port, bolt}
	r.env = map[string]string{"JAVA_HOME": DisplayPath(javaHome), "NEO4J_HOME": DisplayPath(home), "NEO4J_CONF": DisplayPath(r.etc)}
	r.removeEnvironment = []string{"JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH", "NEO4J_OPTS"}
	return nil
}

func (c *Core) prepareRabbitMQ(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	erlang, err := c.rabbitErlang(ctx, e.pkg.Version)
	if err != nil {
		return err
	}
	management, distribution := r.port+10000, r.port+20000
	if distribution > 65535 {
		return invalid("RabbitMQ 派生端口超出范围")
	}
	for _, name := range []string{"mnesia", "log", "home"} {
		if err = managedDirectory(c.Home, filepath.Join(r.data, name)); err != nil {
			return err
		}
	}
	path := filepath.Join(r.etc, "rabbitmq.conf")
	text, err := readConfigText(path)
	if os.IsNotExist(err) {
		text = ""
	} else if err != nil {
		return err
	}
	original := text
	for _, setting := range [][2]string{{"listeners.tcp.default", fmt.Sprintf("127.0.0.1:%d", r.port)}, {"management.tcp.port", strconv.Itoa(management)}, {"management.tcp.ip", "127.0.0.1"}, {"distribution.listener.port_range.min", strconv.Itoa(distribution)}, {"distribution.listener.port_range.max", strconv.Itoa(distribution)}} {
		text = managedConfigValue(text, setting[0], setting[1])
	}
	if text != original {
		if err = c.writeConfig(ctx, configTarget{key: "rabbitmq-config", path: path}, text, nil); err != nil {
			return err
		}
	}
	plugins := filepath.Join(r.etc, "enabled_plugins")
	if _, err = os.Stat(plugins); os.IsNotExist(err) {
		if err = atomicWrite(plugins, []byte("[rabbitmq_management].\n")); err != nil {
			return err
		}
	} else if err != nil {
		return err
	}
	node, found, err := c.Store.setting(ctx, "rabbitNodeName")
	if err != nil {
		return err
	}
	if !found {
		node = rabbitNodeName(c.Home)
		if err = c.Store.saveSetting(ctx, "rabbitNodeName", node); err != nil {
			return err
		}
	}
	if !validRabbitNodeName(node) {
		return invalid("RabbitMQ 节点身份无效，未启动新数据实例")
	}
	r.env = map[string]string{"ERLANG_HOME": DisplayPath(erlang), "RABBITMQ_BASE": DisplayPath(r.data), "RABBITMQ_CONFIG_FILE": DisplayPath(path), "RABBITMQ_ENABLED_PLUGINS_FILE": DisplayPath(plugins), "RABBITMQ_MNESIA_BASE": DisplayPath(filepath.Join(r.data, "mnesia")), "RABBITMQ_LOG_BASE": DisplayPath(filepath.Join(r.data, "log")), "RABBITMQ_NODENAME": node, "RABBITMQ_NODE_PORT": strconv.Itoa(r.port), "RABBITMQ_DIST_PORT": strconv.Itoa(distribution), "RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS": "-kernel inet_dist_use_interface {127,0,0,1}", "ERL_EPMD_ADDRESS": "127.0.0.1", "RABBITMQ_ADVANCED_CONFIG_FILE": DisplayPath(filepath.Join(r.etc, "advanced.config"))}
	// Give the broker and its CLI the same private cookie location.
	r.env["HOME"] = DisplayPath(filepath.Join(r.data, "home"))
	r.env["HOMEDRIVE"] = filepath.VolumeName(r.env["HOME"])
	r.env["HOMEPATH"] = strings.TrimPrefix(r.env["HOME"], r.env["HOMEDRIVE"])
	r.removeEnvironment = []string{"RABBITMQ_SERVER_START_ARGS", "RABBITMQ_SERVER_ERL_ARGS", "RABBITMQ_CTL_ERL_ARGS", "ERL_FLAGS", "ERL_AFLAGS", "ERL_ZFLAGS", "RABBITMQ_USE_LONGNAME", "RABBITMQ_ERLANG_COOKIE", "RABBITMQ_CONF_ENV_FILE"}
	r.args = nil
	r.stopArgs = nil
	r.ports = []int{r.port, management, distribution}
	return nil
}

func rabbitNodeName(home string) string {
	digest := sha256.Sum256([]byte(filepath.Clean(home)))
	return "niceenv_" + hex.EncodeToString(digest[:8]) + "@localhost"
}
func validRabbitNodeName(name string) bool {
	value := strings.TrimSuffix(strings.TrimPrefix(name, "niceenv_"), "@localhost")
	bytes, err := hex.DecodeString(value)
	return strings.HasPrefix(name, "niceenv_") && strings.HasSuffix(name, "@localhost") && err == nil && len(bytes) == 8
}

func (c *Core) rabbitErlang(ctx context.Context, rabbitVersion string) (string, error) {
	views, err := c.packageViews(ctx)
	if err != nil {
		return "", err
	}
	compatible := func(version string) bool {
		major, err := strconv.Atoi(strings.Split(version, ".")[0])
		if err != nil {
			return false
		}
		if compareVersionNumber(rabbitVersion, "4.3.6") >= 0 {
			return major == 27 || major == 28
		}
		if compareVersionNumber(rabbitVersion, "4.0.4") >= 0 {
			return major == 27
		}
		return major == 26
	}
	selected := ""
	active := false
	for _, p := range views {
		version, _ := p["version"].(string)
		if p["id"] != "erlang" || p["install"] == nil || !compatible(version) {
			continue
		}
		isActive := p["active"] == true
		if selected == "" || isActive && !active || isActive == active && compareVersionNumber(version, selected) > 0 {
			selected, active = version, isActive
		}
	}
	if selected == "" {
		return "", &AppError{Code: "SERVICE_DEPENDENCY_VERSION", Message: "已安装的 Erlang 版本不受此 RabbitMQ 支持", Hint: "RabbitMQ 4.3.6 可使用 Erlang 27/28，较早的 RabbitMQ 4.x 请使用 Erlang 27；Erlang 29 暂不兼容。请在套件中心安装兼容版本。"}
	}
	_, program, err := c.installedTool(ctx, "erlang", selected)
	if err != nil {
		return "", err
	}
	return filepath.Dir(filepath.Dir(program)), nil
}

func (c *Core) shutdownRabbitMQ(ctx context.Context, e *serviceEntry) error {
	r := e.resolved
	if r == nil || e.child == nil {
		return invalid("RabbitMQ 进程已变化")
	}
	if err := c.verifyDatabaseListener(ctx, e); err != nil {
		return err
	}
	name := "rabbitmqctl"
	if runtime.GOOS == "windows" {
		name += ".bat"
	}
	program := filepath.Join(filepath.Dir(r.program), name)
	ctx, cancel := context.WithTimeout(ctx, 25*time.Second)
	defer cancel()
	out := process.NewLimitedBuffer(16384)
	child, err := process.Start(ctx, process.Spec{Program: program, Args: []string{"-n", r.env["RABBITMQ_NODENAME"], "shutdown"}, Directory: r.cwd, Environment: r.env, RemoveEnvironment: r.removeEnvironment, Stdout: out, Stderr: out})
	if err != nil {
		return err
	}
	result := child.Wait()
	if result.Err != nil || result.Code != 0 {
		return &AppError{Code: "SERVICE_STOP_FAILED", Message: "RabbitMQ 正常停机失败", Detail: redactDiagnostic(out.String())}
	}
	return nil
}

func (c *Core) prepareElasticsearch(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	home := filepath.Dir(filepath.Dir(r.program))
	if err := managedCopyDefaults(c.Home, filepath.Join(home, "config"), r.etc); err != nil {
		return err
	}
	for _, name := range []string{"tmp", "es-data", "es-logs"} {
		if err := managedDirectory(c.Home, filepath.Join(r.data, name)); err != nil {
			return err
		}
	}
	transport := r.port + 100
	if transport > 65535 {
		return invalid("Elasticsearch 内部通信端口超出范围")
	}
	r.args = []string{"-Epath.data=" + filepath.ToSlash(DisplayPath(filepath.Join(r.data, "es-data"))), "-Epath.logs=" + filepath.ToSlash(DisplayPath(filepath.Join(r.data, "es-logs"))), "-Ehttp.port=" + strconv.Itoa(r.port), "-Etransport.port=" + strconv.Itoa(transport), "-Enetwork.host=127.0.0.1", "-Ediscovery.type=single-node", "-Expack.security.enabled=false", "-Expack.security.enrollment.enabled=false"}
	r.env = map[string]string{"ES_PATH_CONF": DisplayPath(r.etc), "ES_TMPDIR": DisplayPath(filepath.Join(r.data, "tmp")), "ES_JAVA_OPTS": "-Xms256m -Xmx512m"}
	r.removeEnvironment = []string{"ES_JAVA_HOME", "JAVA_HOME", "JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH"}
	r.ports = []int{r.port, transport}
	r.stopArgs = nil
	return nil
}
