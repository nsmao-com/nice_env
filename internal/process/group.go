package process

import (
	"context"
	"errors"
	"sync"
)

// StartGroup gives a pool the same ownership and lifetime semantics as one child.
// A partial launch or unexpected worker exit tears down the remaining workers.
func StartGroup(ctx context.Context, specs []Spec, terminateOnStop bool) (*Child, []int, error) {
	if len(specs) == 0 {
		return nil, nil, errors.New("empty process group")
	}
	children := make([]*Child, 0, len(specs))
	stopAll := func() error {
		var failures []error
		for _, child := range children {
			failures = append(failures, child.Stop())
		}
		return errors.Join(failures...)
	}
	for _, spec := range specs {
		child, err := Start(ctx, spec)
		if err != nil {
			stopErr := stopAll()
			for _, started := range children {
				started.Wait()
			}
			return nil, nil, errors.Join(err, stopErr)
		}
		children = append(children, child)
	}
	pids := make([]int, 0, len(children))
	for _, child := range children {
		pids = append(pids, child.PID)
	}
	group := &Child{PID: children[0].PID, done: make(chan struct{}), terminate: stopAll}
	group.graceful = func() error {
		if terminateOnStop {
			return stopAll()
		}
		var failures []error
		for _, child := range children {
			failures = append(failures, child.RequestStop())
		}
		return errors.Join(failures...)
	}
	group.owns = func(pid int) (bool, error) {
		for _, child := range children {
			owned, err := child.OwnsPID(pid)
			if owned || err != nil {
				return owned, err
			}
		}
		return false, nil
	}
	go func() {
		var wg sync.WaitGroup
		results := make(chan Result, len(children))
		for _, child := range children {
			wg.Add(1)
			go func(child *Child) { defer wg.Done(); results <- child.Wait() }(child)
		}
		first := <-results
		stopErr := stopAll()
		wg.Wait()
		group.result = Result{Code: first.Code, Err: errors.Join(first.Err, stopErr)}
		close(group.done)
	}()
	return group, pids, nil
}
