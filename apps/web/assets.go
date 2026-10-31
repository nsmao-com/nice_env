//go:build desktopassets

package webassets

import (
	"bytes"
	"embed"
	"fmt"
	"io/fs"
	"net/http"
)

// Frontend assets are built explicitly by the release pipeline, never by go build.
//
//go:embed all:out
var bundle embed.FS

func Handler() (http.Handler, error) {
	assets, err := fs.Sub(bundle, "out")
	if err != nil {
		return nil, err
	}
	index, err := fs.ReadFile(assets, "index.html")
	if err != nil {
		return nil, err
	}
	if !bytes.Contains(index, []byte(`name="niceenv-desktop-bridge" content="1"`)) {
		return nil, fmt.Errorf("frontend assets predate the Wails bridge; rebuild web assets explicitly before launching")
	}
	index = bytes.Replace(index, []byte("<head>"), []byte("<head><script>window.__NICEENV_WAILS__=true;</script>"), 1)
	files := http.FileServer(http.FS(assets))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/" || r.URL.Path == "/index.html" {
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			w.Header().Set("Cache-Control", "no-store")
			_, _ = w.Write(index)
			return
		}
		files.ServeHTTP(w, r)
	}), nil
}
