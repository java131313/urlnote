//go:build js && wasm

package main

import (
	"io"
	"net/http"
	"path/filepath"
	"strings"

	"github.com/syumai/workers-go/cloudflare"
	"github.com/syumai/workers-go/cloudflare/fetch"
)

// assetBase is a placeholder host: requests sent through the ASSETS service
// binding are served from the static assets upload, the hostname is ignored.
const assetBase = "https://assets.urlnote/"

// newAssetsHandler serves the SPA shell. Requests matching an uploaded file
// are answered by Cloudflare's asset layer before the worker runs; only
// extensionless client routes (e.g. /u0yYk) reach here, so index.html is
// fetched through the ASSETS binding. /api/* and missing assets 404.
func newAssetsHandler() http.Handler {
	assets := fetch.NewClient(fetch.WithBinding(cloudflare.GetBinding("ASSETS"))).
		HTTPClient(fetch.RedirectModeFollow)

	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead {
			http.NotFound(w, r)
			return
		}
		if strings.HasPrefix(r.URL.Path, "/api/") || filepath.Ext(r.URL.Path) != "" {
			http.NotFound(w, r)
			return
		}
		req, err := http.NewRequestWithContext(r.Context(), http.MethodGet, assetBase+"index.html", nil)
		if err != nil {
			http.Error(w, "internal error", http.StatusInternalServerError)
			return
		}
		resp, err := assets.Do(req)
		if err != nil {
			http.Error(w, "asset fetch failed", http.StatusInternalServerError)
			return
		}
		defer resp.Body.Close()
		for k, vv := range resp.Header {
			for _, v := range vv {
				w.Header().Add(k, v)
			}
		}
		w.WriteHeader(resp.StatusCode)
		_, _ = io.Copy(w, resp.Body)
	})
}
