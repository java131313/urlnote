package api

import (
	"net/http"
	"path"
	"path/filepath"
	"strings"
)

// spaHandler serves the frontend statics with an SPA fallback: real files
// are served as-is; extensionless paths that don't resolve fall back to
// index.html (client routes like /u0yYk); /api/* and missing assets 404.
type spaHandler struct {
	root http.Dir
}

// NewStaticHandler builds the static file handler rooted at dir.
func NewStaticHandler(dir string) http.Handler {
	return &spaHandler{root: http.Dir(dir)}
}

func (h *spaHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		http.NotFound(w, r)
		return
	}
	p := r.URL.Path
	if strings.HasPrefix(p, "/api/") {
		http.NotFound(w, r)
		return
	}
	clean := path.Clean("/" + p)
	if clean == "/" {
		h.serveFile(w, r, "/index.html")
		return
	}
	if f, err := h.root.Open(filepath.FromSlash(clean)); err == nil {
		fi, statErr := f.Stat()
		if statErr == nil && !fi.IsDir() {
			http.ServeContent(w, r, clean, fi.ModTime(), f)
			return
		}
		_ = f.Close()
	}
	// extensionless (or directory) paths fall back to the SPA shell —
	// valid note ids never contain a dot
	if filepath.Ext(clean) == "" {
		h.serveFile(w, r, "/index.html")
		return
	}
	http.NotFound(w, r)
}

func (h *spaHandler) serveFile(w http.ResponseWriter, r *http.Request, name string) {
	f, err := h.root.Open(filepath.FromSlash(name))
	if err != nil {
		http.NotFound(w, r)
		return
	}
	defer func() { _ = f.Close() }()
	fi, err := f.Stat()
	if err != nil {
		http.NotFound(w, r)
		return
	}
	http.ServeContent(w, r, name, fi.ModTime(), f)
}
