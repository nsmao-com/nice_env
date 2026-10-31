package core

import (
	"context"
	"encoding/json"
	"sync"
	"time"

	"github.com/shirou/gopsutil/v4/cpu"
	"github.com/shirou/gopsutil/v4/disk"
	"github.com/shirou/gopsutil/v4/mem"
)

type StatsPoint struct {
	Time   int64   `json:"t"`
	CPU    float64 `json:"cpu"`
	Memory float64 `json:"mem"`
}
type SystemStats struct {
	CPUPercent    float64      `json:"cpuPercent"`
	MemoryUsedMB  float64      `json:"memUsedMb"`
	MemoryTotalMB float64      `json:"memTotalMb"`
	DiskFreeGB    float64      `json:"diskFreeGb"`
	DiskTotalGB   float64      `json:"diskTotalGb"`
	History       []StatsPoint `json:"history"`
}
type statsSampler struct {
	mu      sync.Mutex
	at      time.Time
	value   SystemStats
	pending chan struct{}
	err     error
}

func (c *Core) registerStats() {
	sampler := &statsSampler{}
	c.handle("get_system_stats", func(ctx context.Context, _ json.RawMessage) (any, error) {
		sampler.mu.Lock()
		if time.Since(sampler.at) < 2*time.Second {
			value := sampler.snapshot()
			sampler.mu.Unlock()
			return value, nil
		}
		if sampler.pending == nil {
			sampler.pending = make(chan struct{})
			go sampler.refresh(c.Home)
		}
		done := sampler.pending
		if !sampler.at.IsZero() {
			if time.Since(sampler.at) > 30*time.Second {
				sampler.mu.Unlock()
				return nil, &AppError{Code: "STATS_UNAVAILABLE", Message: "系统采样未及时更新，请稍后重试"}
			}
			value := sampler.snapshot()
			sampler.mu.Unlock()
			return value, nil
		}
		sampler.mu.Unlock()
		ctx, cancel := context.WithTimeout(ctx, 3*time.Second)
		defer cancel()
		select {
		case <-ctx.Done():
			return nil, &AppError{Code: "STATS_UNAVAILABLE", Message: "系统采样暂时未返回，请稍后重试"}
		case <-done:
		}
		sampler.mu.Lock()
		defer sampler.mu.Unlock()
		if sampler.err != nil {
			return nil, &AppError{Code: "STATS_UNAVAILABLE", Message: "系统采样失败，请稍后重试"}
		}
		return sampler.snapshot(), nil
	})
}

func (sampler *statsSampler) refresh(home string) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	var value SystemStats
	collect := func() error {
		// A zero sampling interval reads the delta from the previous call. It
		// never sleeps to sample CPU and never walks the global process list.
		percent, err := cpu.PercentWithContext(ctx, 0, false)
		if err != nil {
			return err
		}
		memory, err := mem.VirtualMemoryWithContext(ctx)
		if err != nil {
			return err
		}
		volume, err := disk.UsageWithContext(ctx, home)
		if err != nil {
			return err
		}
		cpuPercent := float64(0)
		if len(percent) > 0 {
			cpuPercent = percent[0]
		}
		value = SystemStats{CPUPercent: cpuPercent, MemoryUsedMB: float64(memory.Used) / (1024 * 1024), MemoryTotalMB: float64(memory.Total) / (1024 * 1024), DiskFreeGB: float64(volume.Free) / (1024 * 1024 * 1024), DiskTotalGB: float64(volume.Total) / (1024 * 1024 * 1024)}
		return nil
	}
	err := collect()
	sampler.mu.Lock()
	defer sampler.mu.Unlock()
	sampler.err = err
	if err == nil {
		point := StatsPoint{Time: time.Now().Unix(), CPU: value.CPUPercent}
		if value.MemoryTotalMB > 0 {
			point.Memory = value.MemoryUsedMB / value.MemoryTotalMB * 100
		}
		value.History = append(sampler.value.History, point)
		if len(value.History) > 150 {
			value.History = append([]StatsPoint(nil), value.History[len(value.History)-150:]...)
		}
		sampler.value = value
		sampler.at = time.Now()
	}
	close(sampler.pending)
	sampler.pending = nil
}

func (s *statsSampler) snapshot() SystemStats {
	copy := s.value
	copy.History = append([]StatsPoint(nil), s.value.History...)
	return copy
}
