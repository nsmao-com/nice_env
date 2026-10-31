//go:build !desktopassets

package webassets

import (
	"errors"
	"net/http"
)

// Source-only compilation is supported without stale or placeholder UI assets.
func Handler() (http.Handler, error) {
	return nil, errors.New("web assets are not embedded: explicitly build the web frontend, then compile with -tags desktopassets")
}
