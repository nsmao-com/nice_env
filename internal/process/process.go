// Package process owns subprocess trees. It never terminates processes by name
// or by an unverified PID from a previous application session.
package process

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
)

type Spec struct {
	Program           string
	Args              []string
	Directory         string
	Environment       map[string]string
	RemoveEnvironment []string
	Stdin             *os.File
	Stdout            io.Writer
	Stderr            io.Writer
	// CommandLine is only set by Shell for cmd.exe's own quoting rules.
	commandLine string
}

type Result struct {
	Code int
	Err  error
}

type Child struct {
	PID       int
	done      chan struct{}
	result    Result
	terminate func() error
	graceful  func() error
	owns      func(int) (bool, error)
	mu        sync.Mutex
}

var ErrGracefulUnsupported = errors.New("this process has no native graceful signal")

func (p *Child) RequestStop() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	select {
	case <-p.done:
		return nil
	default:
	}
	if p.graceful == nil {
		return ErrGracefulUnsupported
	}
	return p.graceful()
}

func (p *Child) OwnsPID(pid int) (bool, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	select {
	case <-p.done:
		return false, nil
	default:
	}
	return p.owns(pid)
}

func (p *Child) Done() <-chan struct{} { return p.done }
func (p *Child) Wait() Result          { <-p.done; return p.result }
func (p *Child) Stop() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	select {
	case <-p.done:
		return nil
	default:
	}
	return p.terminate()
}

func Start(ctx context.Context, spec Spec) (*Child, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if !filepath.IsAbs(spec.Program) {
		return nil, errors.New("process program must be an absolute path")
	}
	if spec.Directory != "" && !filepath.IsAbs(spec.Directory) {
		return nil, errors.New("process working directory must be absolute")
	}
	for key, value := range spec.Environment {
		if key == "" || strings.ContainsAny(key, "=\x00") || strings.ContainsRune(value, 0) {
			return nil, errors.New("invalid process environment")
		}
	}
	if spec.Stdout == nil {
		spec.Stdout = io.Discard
	}
	if spec.Stderr == nil {
		spec.Stderr = io.Discard
	}
	return start(ctx, spec)
}

func environment(overrides map[string]string, removed []string) []string {
	values := map[string]string{}
	keyOf := func(key string) string {
		if runtime.GOOS == "windows" {
			return strings.ToUpper(key)
		}
		return key
	}
	for _, entry := range os.Environ() {
		if i := strings.IndexByte(entry, '='); i > 0 {
			values[keyOf(entry[:i])] = entry
		}
	}
	for key, value := range overrides {
		values[keyOf(key)] = key + "=" + value
	}
	for _, key := range removed {
		delete(values, keyOf(key))
	}
	result := make([]string, 0, len(values))
	for _, entry := range values {
		result = append(result, entry)
	}
	sort.Slice(result, func(i, j int) bool { return keyOf(result[i]) < keyOf(result[j]) })
	return result
}

// LimitedBuffer keeps output bounded while continuing to drain child pipes.
type LimitedBuffer struct {
	mu      sync.Mutex
	data    []byte
	limit   int
	clipped bool
}

func NewLimitedBuffer(limit int) *LimitedBuffer { return &LimitedBuffer{limit: max(0, limit)} }
func (b *LimitedBuffer) Write(data []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	keep := min(len(data), b.limit-len(b.data))
	b.data = append(b.data, data[:keep]...)
	b.clipped = b.clipped || keep < len(data)
	return len(data), nil
}
func (b *LimitedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	value := strings.ToValidUTF8(string(b.data), "�")
	if b.clipped {
		value += "\n…（输出超过限制，后续内容已省略）"
	}
	return value
}
