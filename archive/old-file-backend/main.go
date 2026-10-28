package main

import (
	"crypto/rand"
	"encoding/json"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/gin-gonic/gin"
)

var addr = ":3007" // PORT env overrides, e.g. PORT=3008

const dataDir = "data"

func init() {
	if p := os.Getenv("PORT"); p != "" {
		addr = ":" + p
	}
}

var idRe = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,64}$`)

// ---- storage ----

var store = struct {
	sync.Mutex
}{}

func notePath(id string) string {
	return filepath.Join(dataDir, id+".txt")
}

func loadNote(id string) (string, error) {
	store.Lock()
	defer store.Unlock()
	b, err := os.ReadFile(notePath(id))
	if os.IsNotExist(err) {
		return "", nil
	}
	return string(b), err
}

func saveNote(id, content string) error {
	store.Lock()
	defer store.Unlock()
	tmp := notePath(id) + ".tmp"
	if err := os.WriteFile(tmp, []byte(content), 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, notePath(id))
}

// ---- handlers ----

func newID() string {
	const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	b := make([]byte, 5)
	if _, err := rand.Read(b); err != nil {
		log.Fatal(err)
	}
	for i := range b {
		b[i] = alphabet[int(b[i])%len(alphabet)]
	}
	return string(b)
}

func validID(c *gin.Context) (string, bool) {
	id := c.Param("id")
	if !idRe.MatchString(id) {
		c.Status(http.StatusNotFound)
		return "", false
	}
	return id, true
}

func handleIndex(c *gin.Context) {
	c.Redirect(http.StatusFound, "/"+newID())
}

func handleNotePage(c *gin.Context) {
	// served via NoRoute, so there is no :id param — take the single path segment
	id := strings.TrimPrefix(c.Request.URL.Path, "/")
	if c.Request.Method != http.MethodGet || !idRe.MatchString(id) {
		c.Status(http.StatusNotFound)
		return
	}
	c.Data(http.StatusOK, "text/html; charset=utf-8", []byte(pageHTML(id)))
}

func handleGetNote(c *gin.Context) {
	id, ok := validID(c)
	if !ok {
		return
	}
	content, err := loadNote(id)
	if err != nil {
		c.String(http.StatusInternalServerError, `{"error":"read failed"}`)
		return
	}
	c.JSON(http.StatusOK, map[string]string{"id": id, "content": content})
}

func handleSaveNote(c *gin.Context) {
	id, ok := validID(c)
	if !ok {
		return
	}
	var body struct {
		Content string `json:"content"`
	}
	// decode the body directly: sendBeacon posts with Content-Type text/plain
	if err := json.NewDecoder(c.Request.Body).Decode(&body); err != nil {
		c.String(http.StatusBadRequest, `{"error":"bad request"}`)
		return
	}
	if err := saveNote(id, body.Content); err != nil {
		c.String(http.StatusInternalServerError, `{"error":"save failed"}`)
		return
	}
	c.Data(http.StatusOK, "application/json; charset=utf-8", []byte(`{"ok":true}`))
}

func main() {
	if err := os.MkdirAll(dataDir, 0o755); err != nil {
		log.Fatal(err)
	}

	r := gin.Default()
	r.GET("/", handleIndex)
	r.GET("/api/note/:id", handleGetNote)
	r.PUT("/api/note/:id", handleSaveNote)
	r.POST("/api/note/:id", handleSaveNote) // sendBeacon on page unload
	// gin forbids a static segment ("api") next to a wildcard (":id") at the
	// same tree level, so single-segment note pages are served via NoRoute.
	r.NoRoute(handleNotePage)

	server := &http.Server{
		Addr:              addr,
		Handler:           r,
		ReadHeaderTimeout: 10 * time.Second,
	}
	log.Printf("urlnote listening on http://localhost%s", addr)
	log.Fatal(server.ListenAndServe())
}
