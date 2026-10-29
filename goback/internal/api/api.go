// Package api implements the urlnote HTTP API: a Go port of the Rust
// worker's lib.rs backed by GORM/PostgreSQL. The JSON contract, status codes
// and cache behavior mirror the Rust worker (and the original Go backend
// before it), so existing clients work unchanged.
package api

import (
	"encoding/json"
	"net/http"
	"regexp"
	"time"

	"gorm.io/gorm"

	"urlnote/goback/internal/cache"
	"urlnote/goback/internal/token"
	"urlnote/goback/shared"
)

// cache key layout matches the Rust/Go workers exactly (v4 payloads).
const (
	noteKeyPrefix    = "v4/notes/"
	indexKey         = "v4/index.json"
	cacheContentType = "application/json; charset=utf-8"
)

// presence/sessions TTLs (mirroring the Rust worker).
const (
	sessionTTLSec  = 7 * 24 * 3600
	presenceTTLSec = 90
)

var (
	idRe        = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,64}$`)
	colorRe     = regexp.MustCompile(`^#[0-9a-fA-F]{6}$`)
	tokenRe     = regexp.MustCompile(`^[a-zA-Z0-9]{32}$`)
	imageNameRe = regexp.MustCompile(`^[a-zA-Z0-9]{16}\.[a-z0-9]+$`)
)

// Server wires the database, segmenters, cache and static assets.
type Server struct {
	DB       *gorm.DB
	Ana      *token.Analyzer
	Cache    *cache.Cache
	CORS     []string
	ImageDir string
	Static   http.Handler // serves the WASM SPA + assets (with index fallback)
}

func noteKey(id string) string { return noteKeyPrefix + id + ".json" }

// nowStr formats time like D1's datetime('now'): UTC "YYYY-MM-DD HH:MM:SS".
// The migrated schema keeps timestamps as TEXT, so the Go server must
// produce the identical format.
func nowStr() string {
	return time.Now().UTC().Format("2006-01-02 15:04:05")
}

// timeNowAdd returns now+d seconds in the same TEXT timestamp format.
func timeNowAdd(d time.Duration) string {
	return time.Now().UTC().Add(d * time.Second).Format("2006-01-02 15:04:05")
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", cacheContentType)
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func jsonError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, shared.ErrorResponse{Error: msg})
}

// writeRawJSON writes a pre-serialized payload with the shared content type.
func (s *Server) writeRawJSON(w http.ResponseWriter, status int, payload []byte, xCache string) {
	w.Header().Set("Content-Type", cacheContentType)
	if xCache != "" {
		w.Header().Set("X-Cache", xCache)
	}
	w.WriteHeader(status)
	_, _ = w.Write(payload)
}

// Routes returns the configured mux.
func (s *Server) Routes() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /api/note/{id}", s.getNote)
	mux.HandleFunc("HEAD /api/note/{id}", s.getNote)
	mux.HandleFunc("PUT /api/note/{id}", s.saveNote)
	mux.HandleFunc("POST /api/note/{id}", s.saveNote) // POST kept for API compatibility (was sendBeacon)
	mux.HandleFunc("GET /api/notes", s.listNotes)
	mux.HandleFunc("POST /api/login", s.login)
	mux.HandleFunc("POST /api/logout", s.logout)
	mux.HandleFunc("GET /api/whoami", s.whoami)
	mux.HandleFunc("POST /api/presence", s.presence)
	mux.HandleFunc("POST /api/upload", s.upload)
	mux.HandleFunc("GET /api/edit-status", s.editStatus)
	mux.HandleFunc("GET /api/search", s.searchNotes)
	mux.HandleFunc("POST /api/analyze", s.analyzeText)
	mux.HandleFunc("POST /api/pos", s.posTag)
	mux.HandleFunc("POST /api/pos/doc", s.posDocument)
	mux.HandleFunc("POST /api/mark/check", s.checkMarks)
	mux.HandleFunc("POST /api/mark/{id}", s.generateMark)
	mux.HandleFunc("PUT /api/mark/{id}", s.saveMark)
	mux.HandleFunc("DELETE /api/mark/{id}", s.removeMark)
	mux.HandleFunc("GET /api/mark/{id}", s.getMark)
	mux.HandleFunc("POST /api/search/reindex", s.reindexSearch)
	mux.HandleFunc("POST /api/edit-lock", s.setEditLock)
	mux.HandleFunc("POST /api/edit-request", s.submitEditRequest)
	mux.HandleFunc("GET /api/edit-requests", s.listEditRequests)
	mux.HandleFunc("POST /api/edit-requests/handle", s.handleEditRequest)
	mux.HandleFunc("GET /i/{name}", s.image)
	mux.HandleFunc("/", s.serveApp)
	return mux
}

// Handler wraps the mux with CORS handling. CORS headers must be decided
// before the handler runs (they cannot be added after WriteHeader), so the
// origin check happens up front.
func (s *Server) Handler() http.Handler {
	mux := s.Routes()
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		isAPI := len(r.URL.Path) >= 4 && r.URL.Path[:4] == "/api"
		if r.Method == http.MethodOptions && isAPI {
			s.corsPreflight(w, r)
			return
		}
		origin := r.Header.Get("Origin")
		if isAPI && origin != "" {
			s.applyCORS(origin, w)
		}
		mux.ServeHTTP(w, r)
	})
}

// applyCORS sets CORS headers when the request origin is allowed.
func (s *Server) applyCORS(origin string, w http.ResponseWriter) {
	for _, o := range s.CORS {
		if o == origin {
			h := w.Header()
			h.Set("Access-Control-Allow-Origin", origin)
			h.Set("Access-Control-Allow-Credentials", "true")
			h.Set("Vary", "Origin")
			return
		}
	}
}

func (s *Server) corsPreflight(w http.ResponseWriter, r *http.Request) {
	origin := r.Header.Get("Origin")
	for _, o := range s.CORS {
		if o == origin {
			h := w.Header()
			h.Set("Access-Control-Allow-Origin", origin)
			h.Set("Access-Control-Allow-Credentials", "true")
			h.Set("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
			allow := r.Header.Get("Access-Control-Request-Headers")
			if allow == "" {
				allow = "Content-Type"
			}
			h.Set("Access-Control-Allow-Headers", allow)
			h.Set("Access-Control-Max-Age", "86400")
			w.WriteHeader(http.StatusNoContent)
			return
		}
	}
	w.WriteHeader(http.StatusNoContent)
}

// requireUser guards a handler like the Rust macro: on failure it writes 401
// and returns ok=false.
func (s *Server) requireUser(w http.ResponseWriter, r *http.Request) (string, bool) {
	u, ok := s.currentUser(r)
	if !ok {
		jsonError(w, http.StatusUnauthorized, "unauthorized")
		return "", false
	}
	return u, true
}
