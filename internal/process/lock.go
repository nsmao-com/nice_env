package process

import "errors"

var ErrLocked = errors.New("operation is already running")
