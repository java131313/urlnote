package api

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"strings"
	"time"

	"urlnote/goback/shared"
)

// Password hashes are SHA-256 of "username:password" (hex), seeded by
// migrations/0003-users.sql — there is deliberately no registration.
// Sessions live in the D1 sessions table (migrated from KV).

func passwordHash(username, password string) string {
	sum := sha256.Sum256([]byte(username + ":" + password))
	return hex.EncodeToString(sum[:])
}

func newToken() string {
	const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		panic("crypto/rand: " + err.Error())
	}
	for i := range b {
		b[i] = alphabet[int(b[i])%len(alphabet)]
	}
	return string(b)
}

func isHTTPS(r *http.Request) bool {
	return r.TLS != nil ||
		r.Header.Get("X-Forwarded-Proto") == "https" ||
		strings.Contains(r.Header.Get("CF-Visitor"), `"https"`)
}

func (s *Server) currentUser(r *http.Request) (string, bool) {
	c, err := r.Cookie("session")
	if err != nil || !validToken(c.Value) {
		return "", false
	}
	username, ok, err := s.DB.GetSessionUsername(r.Context(), c.Value, nowStr())
	if err != nil || !ok {
		return "", false
	}
	return username, true
}

func (s *Server) login(w http.ResponseWriter, r *http.Request) {
	var body shared.LoginRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	username := strings.TrimSpace(body.Username)

	hash, found, err := s.DB.GetPasswordHash(r.Context(), username)
	if err != nil || !found || !hmac.Equal([]byte(passwordHash(username, body.Password)), []byte(hash)) {
		jsonError(w, http.StatusUnauthorized, "用户名或密码错误")
		return
	}

	// Expired rows are purged opportunistically on each login.
	_ = s.DB.DeleteExpiredSessions(r.Context(), nowStr())

	token := newToken()
	expires := time.Now().UTC().Add(sessionTTLSec * time.Second).Format("2006-01-02 15:04:05")
	if err := s.DB.CreateSession(r.Context(), token, username, expires); err != nil {
		jsonError(w, http.StatusInternalServerError, "session failed")
		return
	}

	// SameSite=None is required for cross-origin deployments (frontend on
	// another origin): browsers only attach the session cookie to cross-site
	// fetches with SameSite=None, which in turn requires Secure (HTTPS).
	cookie := http.Cookie{
		Name:     "session",
		Value:    token,
		Path:     "/",
		MaxAge:   sessionTTLSec,
		HttpOnly: true,
	}
	if isHTTPS(r) {
		cookie.Secure = true
		cookie.SameSite = http.SameSiteNoneMode
	} else {
		// local dev over plain http
		cookie.SameSite = http.SameSiteLaxMode
	}
	http.SetCookie(w, &cookie)
	writeJSON(w, http.StatusOK, shared.LoginResponse{OK: true, User: shared.User{Username: username}})
}

func (s *Server) logout(w http.ResponseWriter, r *http.Request) {
	if c, err := r.Cookie("session"); err == nil && validToken(c.Value) {
		_ = s.DB.DeleteSession(r.Context(), c.Value)
	}
	http.SetCookie(w, &http.Cookie{Name: "session", Value: "", Path: "/", MaxAge: -1})
	writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
}

func (s *Server) whoami(w http.ResponseWriter, r *http.Request) {
	u, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	writeJSON(w, http.StatusOK, shared.WhoamiResponse{User: shared.User{Username: u}})
}
