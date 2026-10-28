// Command urlnote runs the urlnote app entirely on Cloudflare:
// the Go binary is compiled to WebAssembly and executed by a Cloudflare
// Worker (via github.com/syumai/workers-go). It serves the JSON note API
// backed by a D1 database and falls back to index.html — fetched through the
// Workers Static Assets binding — for the Vue frontend's client-side routes.
package main

import (
	"bytes"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"log"
	"math"
	"net/http"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/syumai/workers-go"
	"github.com/syumai/workers-go/cloudflare"
	_ "github.com/syumai/workers-go/cloudflare/d1" // register the "d1" database/sql driver
	"github.com/syumai/workers-go/cloudflare/fetch"
	"github.com/syumai/workers-go/cloudflare/kv"
)

var (
	idRe = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,64}$`)
	// editor text color, empty means "follow the site theme"
	colorRe = regexp.MustCompile(`^#[0-9a-fA-F]{6}$`)
)

// editor setting bounds — the frontend clamps to the same range
const (
	defaultFontSize = 16
	minFontSize     = 10
	maxFontSize     = 40
)

// assetBase is only a placeholder: requests sent through the ASSETS service
// binding are served from the static assets upload, the hostname is ignored.
const assetBase = "https://assets.urlnote/"

// ---- KV response cache (kept in sync with D1 by write-through) ----

// noteKeyPrefix is versioned: v4 payloads gained updated_at (author was v3,
// title/tags v2); the version bump keeps stale-shaped entries from being served.
const noteKeyPrefix = "v4/notes/"
const indexKey = "v4/index.json"
const cacheContentType = "application/json; charset=utf-8"

func noteKey(id string) string {
	return noteKeyPrefix + id + ".json"
}

// noteJSON serializes the API representation exactly as getNote returns it,
// so the cached bytes and a fresh database response are identical.
func noteJSON(id, content string, fontSize int, color, title string, tags []string, author, updatedAt string) []byte {
	b, _ := json.Marshal(map[string]any{
		"id": id, "content": content, "font_size": fontSize, "color": color,
		"title": title, "tags": tags, "author": author, "updated_at": updatedAt,
	})
	return b
}

// getCachedRaw returns the cached JSON payload for a key, ok=false on miss.
// KV's JS API resolves null/undefined on a miss, which GetString renders as
// the placeholder strings "<null>"/"<undefined>"; a real payload is a JSON
// object, so requiring a leading '{' distinguishes the two unambiguously.
func getCachedRaw(ns *kv.Namespace, key string) ([]byte, bool) {
	if ns == nil {
		return nil, false
	}
	s, err := ns.GetString(key, nil)
	if err != nil || !strings.HasPrefix(s, "{") {
		return nil, false
	}
	return []byte(s), true
}

// writeCacheRaw stores a payload in KV; on failure it deletes the key so the
// next read falls back to D1 instead of serving stale data.
func writeCacheRaw(ns *kv.Namespace, key string, payload []byte) {
	if ns == nil {
		return
	}
	if err := ns.PutString(key, string(payload), nil); err != nil {
		log.Printf("kv put %s failed: %v (deleting key)", key, err)
		_ = ns.Delete(key)
	}
}

func getCachedNote(ns *kv.Namespace, id string) ([]byte, bool) {
	return getCachedRaw(ns, noteKey(id))
}

func writeNoteCache(ns *kv.Namespace, id string, payload []byte) {
	writeCacheRaw(ns, noteKey(id), payload)
}

// cleanTags normalizes user-provided tags: trimmed, deduplicated, capped at
// 10 entries, each at most 24 characters.
func cleanTags(tags []string) []string {
	out := make([]string, 0, len(tags))
	seen := map[string]bool{}
	for _, t := range tags {
		t = strings.TrimSpace(t)
		if t == "" || seen[t] {
			continue
		}
		r := []rune(t)
		if len(r) > 24 {
			t = string(r[:24])
		}
		seen[t] = true
		out = append(out, t)
		if len(out) >= 10 {
			break
		}
	}
	return out
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

// ---- users & sessions ----
//
// Password hashes are SHA-256 of "username:password" (hex), seeded by
// migrations/0003-users.sql — there is deliberately no registration.
// Sessions are opaque 32-char tokens stored in KV ("sess/<token>" →
// username) with a 7-day TTL, carried in an HttpOnly cookie.

const sessionKeyPrefix = "sess/"
const sessionTTLSec = 7 * 24 * 3600

var tokenRe = regexp.MustCompile(`^[a-zA-Z0-9]{32}$`)

func passwordHash(username, password string) string {
	sum := sha256.Sum256([]byte(username + ":" + password))
	return hex.EncodeToString(sum[:])
}

func newToken() string {
	const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		log.Fatalf("crypto/rand: %v", err)
	}
	for i := range b {
		b[i] = alphabet[int(b[i])%len(alphabet)]
	}
	return string(b)
}

func isHTTPS(r *http.Request) bool {
	return r.TLS != nil || strings.Contains(r.Header.Get("CF-Visitor"), `"https"`)
}

func currentUser(r *http.Request, cache *kv.Namespace) (string, bool) {
	if cache == nil {
		return "", false
	}
	c, err := r.Cookie("session")
	if err != nil || !tokenRe.MatchString(c.Value) {
		return "", false
	}
	s, err := cache.GetString(sessionKeyPrefix+c.Value, nil)
	if err != nil || s == "<null>" || s == "<undefined>" {
		return "", false
	}
	return s, true
}

// requireUser guards a handler: on failure it writes 401 and returns ok=false.
func requireUser(w http.ResponseWriter, r *http.Request, cache *kv.Namespace) (string, bool) {
	u, ok := currentUser(r, cache)
	if !ok {
		writeJSON(w, http.StatusUnauthorized, map[string]string{"error": "unauthorized"})
		return "", false
	}
	return u, true
}

func handleLogin(w http.ResponseWriter, r *http.Request, db *sql.DB, cache *kv.Namespace) {
	if cache == nil {
		writeJSON(w, http.StatusServiceUnavailable, map[string]string{"error": "session storage unavailable"})
		return
	}
	var body struct {
		Username string `json:"username"`
		Password string `json:"password"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "bad request"})
		return
	}
	username := strings.TrimSpace(body.Username)
	var storedHash string
	err := db.QueryRowContext(r.Context(),
		`SELECT password_hash FROM users WHERE username = ?`, username).Scan(&storedHash)
	if err != nil || !hmac.Equal([]byte(passwordHash(username, body.Password)), []byte(storedHash)) {
		writeJSON(w, http.StatusUnauthorized, map[string]string{"error": "用户名或密码错误"})
		return
	}
	token := newToken()
	if err := cache.PutString(sessionKeyPrefix+token, username, &kv.PutOptions{ExpirationTTL: sessionTTLSec}); err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "session failed"})
		return
	}
	http.SetCookie(w, &http.Cookie{
		Name:     "session",
		Value:    token,
		Path:     "/",
		MaxAge:   sessionTTLSec,
		HttpOnly: true,
		SameSite: http.SameSiteLaxMode,
		Secure:   isHTTPS(r),
	})
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "user": map[string]string{"username": username}})
}

func handleLogout(w http.ResponseWriter, r *http.Request, cache *kv.Namespace) {
	if cache != nil {
		if c, err := r.Cookie("session"); err == nil && tokenRe.MatchString(c.Value) {
			_ = cache.Delete(sessionKeyPrefix + c.Value)
		}
	}
	http.SetCookie(w, &http.Cookie{Name: "session", Value: "", Path: "/", MaxAge: -1})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func handleWhoami(w http.ResponseWriter, r *http.Request, cache *kv.Namespace) {
	u, ok := currentUser(r, cache)
	if !ok {
		writeJSON(w, http.StatusUnauthorized, map[string]string{"error": "unauthorized"})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"user": map[string]string{"username": u}})
}

// ---- presence: who is looking at a note (heartbeat, TTL in KV) ----

const presenceKeyPrefix = "presence/"
// KV requires TTLs of at least 60 seconds; heartbeats arrive every 15s,
// so 90s still expires ~75s after a client goes away.
const presenceTTLSec = 90

// handlePresence records the caller's visit to a note (TTL refreshes on each
// call) and returns everyone currently present on it.
func handlePresence(w http.ResponseWriter, r *http.Request, cache *kv.Namespace) {
	user, ok := requireUser(w, r, cache)
	if !ok {
		return
	}
	if cache == nil {
		writeJSON(w, http.StatusServiceUnavailable, map[string]string{"error": "presence unavailable"})
		return
	}
	var body struct {
		NoteID string `json:"note_id"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil || !idRe.MatchString(body.NoteID) {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "bad request"})
		return
	}
	prefix := presenceKeyPrefix + body.NoteID + "/"
	if err := cache.PutString(prefix+user, user, &kv.PutOptions{ExpirationTTL: presenceTTLSec}); err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "presence failed"})
		return
	}
	users := []string{}
	if res, err := cache.List(&kv.ListOptions{Prefix: prefix, Limit: 100}); err == nil {
		for _, k := range res.Keys {
			if i := strings.LastIndex(k.Name, "/"); i >= 0 {
				users = append(users, k.Name[i+1:])
			}
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"users": users})
}

func getNote(w http.ResponseWriter, r *http.Request, db *sql.DB, cache *kv.Namespace) {
	id := r.PathValue("id")
	if !idRe.MatchString(id) {
		http.NotFound(w, r)
		return
	}
	if _, ok := requireUser(w, r, cache); !ok {
		return
	}

	// cache hit: serve the KV copy directly
	if payload, ok := getCachedNote(cache, id); ok {
		w.Header().Set("Content-Type", cacheContentType)
		w.Header().Set("X-Cache", "HIT")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write(payload)
		return
	}

	var content, color, title, tagsJSON, author, updatedAt string
	var fontSize int
	err := db.QueryRowContext(r.Context(),
		`SELECT content, font_size, color, title, tags, author, updated_at FROM notes WHERE id = ?`, id).
		Scan(&content, &fontSize, &color, &title, &tagsJSON, &author, &updatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		payload := noteJSON(id, "", defaultFontSize, "", "", nil, "", "")
		writeNoteCache(cache, id, payload)
		w.Header().Set("X-Cache", "MISS")
		_, _ = w.Write(payload)
		return
	}
	if err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "read failed"})
		return
	}
	payload := noteJSON(id, content, fontSize, color, title, parseTags(tagsJSON), author, updatedAt)
	writeNoteCache(cache, id, payload)
	w.Header().Set("X-Cache", "MISS")
	w.Header().Set("Content-Type", cacheContentType)
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(payload)
}

func saveNote(w http.ResponseWriter, r *http.Request, db *sql.DB, cache *kv.Namespace) {
	id := r.PathValue("id")
	if !idRe.MatchString(id) {
		http.NotFound(w, r)
		return
	}
	user, ok := requireUser(w, r, cache)
	if !ok {
		return
	}
	// decode the body directly: sendBeacon posts without fetch-style headers.
	// The body carries the full note state; omitted settings fall back to defaults.
	var body struct {
		Content  string   `json:"content"`
		FontSize float64  `json:"font_size"`
		Color    string   `json:"color"`
		Title    string   `json:"title"`
		Tags     []string `json:"tags"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "bad request"})
		return
	}
	fontSize := defaultFontSize
	if body.FontSize > 0 {
		fontSize = max(minFontSize, min(maxFontSize, int(math.Round(body.FontSize))))
	}
	color := body.Color
	if !colorRe.MatchString(color) {
		color = ""
	}
	title := strings.TrimSpace(body.Title)
	if r := []rune(title); len(r) > 200 {
		title = string(r[:200])
	}
	tags := cleanTags(body.Tags)
	tagsJSON, _ := json.Marshal(tags)
	var updatedAt string
	err := db.QueryRowContext(r.Context(), `INSERT INTO notes (id, content, font_size, color, title, tags, author, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
		ON CONFLICT(id) DO UPDATE SET content = excluded.content,
			font_size = excluded.font_size, color = excluded.color,
			title = excluded.title, tags = excluded.tags,
			author = excluded.author, updated_at = CURRENT_TIMESTAMP
		RETURNING updated_at`,
		id, body.Content, fontSize, color, title, string(tagsJSON), user).Scan(&updatedAt)
	if err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "save failed"})
		return
	}
	// write-through: keep the KV caches (note + index) in sync with D1.
	// The full payload (incl. updated_at/author) is echoed back so clients can
	// sync without an extra GET.
	payload := noteJSON(id, body.Content, fontSize, color, title, tags, user, updatedAt)
	writeNoteCache(cache, id, payload)
	rebuildIndex(r, db, cache)
	w.Header().Set("Content-Type", cacheContentType)
	_, _ = w.Write(payload)
}

// ---- note index (powers the searchable "all notes" dropdown) ----

type noteMeta struct {
	ID        string   `json:"id"`
	Title     string   `json:"title"`
	Tags      []string `json:"tags"`
	UpdatedAt string   `json:"updated_at"`
	Excerpt   string   `json:"excerpt"`
	Author    string   `json:"author"`
}

func parseTags(s string) []string {
	var tags []string
	if err := json.Unmarshal([]byte(s), &tags); err != nil {
		return []string{}
	}
	return tags
}

// queryNoteMeta loads metadata of all notes, newest first.
func queryNoteMeta(r *http.Request, db *sql.DB) ([]noteMeta, error) {
	rows, err := db.QueryContext(r.Context(),
		`SELECT id, title, tags, updated_at, substr(content, 1, 120), author FROM notes ORDER BY updated_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	metas := []noteMeta{}
	for rows.Next() {
		var m noteMeta
		var tagsJSON string
		if err := rows.Scan(&m.ID, &m.Title, &tagsJSON, &m.UpdatedAt, &m.Excerpt, &m.Author); err != nil {
			return nil, err
		}
		m.Tags = parseTags(tagsJSON)
		metas = append(metas, m)
	}
	return metas, rows.Err()
}

// rebuildIndex refreshes the cached note list after a write; on failure the
// key is deleted so the next list request falls back to D1.
func rebuildIndex(r *http.Request, db *sql.DB, cache *kv.Namespace) {
	metas, err := queryNoteMeta(r, db)
	if err != nil {
		log.Printf("rebuild index query failed: %v", err)
		_ = cache.Delete(indexKey)
		return
	}
	payload, _ := json.Marshal(map[string]any{"notes": metas})
	writeCacheRaw(cache, indexKey, payload)
}

func listNotes(w http.ResponseWriter, r *http.Request, db *sql.DB, cache *kv.Namespace) {
	if _, ok := requireUser(w, r, cache); !ok {
		return
	}
	if payload, ok := getCachedRaw(cache, indexKey); ok {
		w.Header().Set("Content-Type", cacheContentType)
		w.Header().Set("X-Cache", "HIT")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write(payload)
		return
	}
	metas, err := queryNoteMeta(r, db)
	if err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "list failed"})
		return
	}
	payload, _ := json.Marshal(map[string]any{"notes": metas})
	writeCacheRaw(cache, indexKey, payload)
	w.Header().Set("X-Cache", "MISS")
	w.Header().Set("Content-Type", cacheContentType)
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(payload)
}

// ---- image upload & serving (images are stored in KV) ----

// maxUploadBytes caps a single uploaded image at 8 MiB (KV values allow 25 MiB).
const maxUploadBytes = 8 << 20

const imageKeyPrefix = "img/"

var imageNameRe = regexp.MustCompile(`^[a-zA-Z0-9]{16}\.[a-z0-9]+$`)

// sniffImage detects the real format by magic bytes — the client-supplied
// content-type is not trustworthy. SVG is deliberately rejected: served
// same-origin it could carry scripts.
func sniffImage(b []byte) (mime, ext string, ok bool) {
	switch {
	case len(b) >= 8 && bytes.Equal(b[:8], []byte("\x89PNG\r\n\x1a\n")):
		return "image/png", ".png", true
	case len(b) >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF:
		return "image/jpeg", ".jpg", true
	case len(b) >= 6 && (string(b[:6]) == "GIF87a" || string(b[:6]) == "GIF89a"):
		return "image/gif", ".gif", true
	case len(b) >= 12 && string(b[:4]) == "RIFF" && string(b[8:12]) == "WEBP":
		return "image/webp", ".webp", true
	case len(b) >= 12 && string(b[4:8]) == "ftyp" && (string(b[8:12]) == "avif" || string(b[8:12]) == "avis"):
		return "image/avif", ".avif", true
	}
	return "", "", false
}

func newImageKey() string {
	const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		log.Fatalf("crypto/rand: %v", err)
	}
	for i := range b {
		b[i] = alphabet[int(b[i])%len(alphabet)]
	}
	return imageKeyPrefix + string(b)
}

// handleUpload accepts Vditor's multipart upload (field "file[]") and stores
// valid images in KV. The response follows Vditor's protocol:
// {msg, code, data: {errFiles, succMap}}.
func handleUpload(w http.ResponseWriter, r *http.Request, cache *kv.Namespace) {
	if _, ok := requireUser(w, r, cache); !ok {
		return
	}
	if cache == nil {
		writeJSON(w, http.StatusServiceUnavailable, map[string]string{"error": "storage unavailable"})
		return
	}
	// headroom for multipart overhead beyond the raw file limit
	r.Body = http.MaxBytesReader(w, r.Body, maxUploadBytes+1<<20)
	if err := r.ParseMultipartForm(maxUploadBytes + 1<<20); err != nil {
		writeJSON(w, http.StatusRequestEntityTooLarge, map[string]string{"error": "upload too large"})
		return
	}
	succMap := map[string]string{}
	errFiles := []string{}
	for _, field := range []string{"file[]", "file"} {
		for _, fh := range r.MultipartForm.File[field] {
			if fh.Size > maxUploadBytes {
				errFiles = append(errFiles, fh.Filename)
				continue
			}
			f, err := fh.Open()
			if err != nil {
				errFiles = append(errFiles, fh.Filename)
				continue
			}
			b, err := io.ReadAll(f)
			f.Close()
			if err != nil || len(b) == 0 {
				errFiles = append(errFiles, fh.Filename)
				continue
			}
			_, ext, ok := sniffImage(b)
			if !ok {
				errFiles = append(errFiles, fh.Filename)
				continue
			}
			key := newImageKey() + ext
			// PutReader (not PutString): the string API round-trips through
			// UTF-8 and would corrupt binary image data
			if err := cache.PutReader(key, bytes.NewReader(b), nil); err != nil {
				log.Printf("kv put %s failed: %v", key, err)
				errFiles = append(errFiles, fh.Filename)
				continue
			}
			succMap[fh.Filename] = "/i/" + strings.TrimPrefix(key, imageKeyPrefix)
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"msg":  "",
		"code": 0,
		"data": map[string]any{"errFiles": errFiles, "succMap": succMap},
	})
}

var imageExtMime = map[string]string{
	".png":  "image/png",
	".jpg":  "image/jpeg",
	".gif":  "image/gif",
	".webp": "image/webp",
	".avif": "image/avif",
}

// handleImage serves an uploaded image from KV with immutable caching —
// keys are random, so content never changes under the same URL.
func handleImage(w http.ResponseWriter, r *http.Request, cache *kv.Namespace) {
	if _, ok := requireUser(w, r, cache); !ok {
		return
	}
	name := r.PathValue("name")
	if !imageNameRe.MatchString(name) || cache == nil {
		http.NotFound(w, r)
		return
	}
	mime, ok := imageExtMime[filepath.Ext(name)]
	if !ok {
		http.NotFound(w, r)
		return
	}
	// probe existence first: GetReader on a miss returns a null stream whose
	// first Read panics
	key := imageKeyPrefix + name
	list, err := cache.List(&kv.ListOptions{Prefix: key, Limit: 1})
	if err != nil || len(list.Keys) == 0 || list.Keys[0].Name != key {
		http.NotFound(w, r)
		return
	}
	reader, err := cache.GetReader(key, nil)
	if err != nil {
		http.NotFound(w, r)
		return
	}
	w.Header().Set("Content-Type", mime)
	w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
	w.Header().Set("X-Cache", "HIT")
	w.WriteHeader(http.StatusOK)
	// the response writer streams the raw ReadableStream back untouched
	_, _ = io.Copy(w, reader)
}

// serveApp serves the Vue SPA: any GET that is not an API call and has no
// file extension is a client-side route (a note id like /u0yYk) and gets
// index.html. Paths with an extension are treated as asset requests and 404
// when missing — valid note ids never contain a dot.
func serveApp(w http.ResponseWriter, r *http.Request, assets *http.Client) {
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
}

func main() {
	// "DB" is the D1 binding name from wrangler.toml. The binding is resolved
	// per request, so sql.Open must stay inside the worker runtime.
	db, err := sql.Open("d1", "DB")
	if err != nil {
		log.Fatalf("open D1 binding DB: %v", err)
	}

	// KV response cache (binding "CACHE"). If the binding is missing, the API
	// transparently falls back to serving everything from D1.
	cache, err := kv.NewNamespace("CACHE")
	if err != nil {
		log.Printf("KV binding CACHE unavailable, running without cache: %v", err)
		cache = nil
	}

	assets := fetch.NewClient(fetch.WithBinding(cloudflare.GetBinding("ASSETS"))).
		HTTPClient(fetch.RedirectModeFollow)

	mux := http.NewServeMux()
	mux.HandleFunc("GET /api/note/{id}", func(w http.ResponseWriter, r *http.Request) {
		getNote(w, r, db, cache)
	})
	mux.HandleFunc("PUT /api/note/{id}", func(w http.ResponseWriter, r *http.Request) {
		saveNote(w, r, db, cache)
	})
	mux.HandleFunc("POST /api/note/{id}", func(w http.ResponseWriter, r *http.Request) {
		saveNote(w, r, db, cache) // sendBeacon on page unload
	})
	mux.HandleFunc("GET /api/notes", func(w http.ResponseWriter, r *http.Request) {
		listNotes(w, r, db, cache)
	})
	mux.HandleFunc("POST /api/login", func(w http.ResponseWriter, r *http.Request) {
		handleLogin(w, r, db, cache)
	})
	mux.HandleFunc("POST /api/logout", func(w http.ResponseWriter, r *http.Request) {
		handleLogout(w, r, cache)
	})
	mux.HandleFunc("GET /api/whoami", func(w http.ResponseWriter, r *http.Request) {
		handleWhoami(w, r, cache)
	})
	mux.HandleFunc("POST /api/presence", func(w http.ResponseWriter, r *http.Request) {
		handlePresence(w, r, cache)
	})
	mux.HandleFunc("POST /api/upload", func(w http.ResponseWriter, r *http.Request) {
		handleUpload(w, r, cache)
	})
	mux.HandleFunc("GET /i/{name}", func(w http.ResponseWriter, r *http.Request) {
		handleImage(w, r, cache)
	})
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		serveApp(w, r, assets)
	})

	workers.Serve(mux)
}
