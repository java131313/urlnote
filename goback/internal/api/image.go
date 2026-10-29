package api

import (
	"bytes"
	"crypto/rand"
	"io"
	"net/http"
	"os"
	"path/filepath"

	"urlnote/goback/shared"
)

// ---- image upload & serving (images are stored on local disk) ----

const (
	maxUploadBytes = 8 << 20
	// headroom for multipart overhead beyond the raw file limit
	uploadHeadroom = 1 << 20
	imageKeyPrefix = "img/"
)

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

func newImageStem() string {
	const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		panic("crypto/rand: " + err.Error())
	}
	for i := range b {
		b[i] = alphabet[int(b[i])%len(alphabet)]
	}
	return string(b)
}

var imageExtMime = map[string]string{
	".png":  "image/png",
	".jpg":  "image/jpeg",
	".gif":  "image/gif",
	".webp": "image/webp",
	".avif": "image/avif",
}

// upload accepts Vditor's multipart upload (field "file[]") and stores valid
// images on disk under ImageDir. The response follows Vditor's protocol:
// {msg, code, data: {errFiles, succMap}}.
func (s *Server) upload(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	if !s.canEditNow(user) {
		jsonError(w, http.StatusForbidden, "edit locked")
		return
	}
	if s.ImageDir == "" {
		jsonError(w, http.StatusServiceUnavailable, "storage unavailable")
		return
	}

	r.Body = http.MaxBytesReader(w, r.Body, maxUploadBytes+uploadHeadroom)
	if err := r.ParseMultipartForm(maxUploadBytes + uploadHeadroom); err != nil {
		jsonError(w, http.StatusRequestEntityTooLarge, "upload too large")
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
			name := newImageStem() + ext
			if err := os.WriteFile(filepath.Join(s.ImageDir, name), b, 0o644); err != nil {
				errFiles = append(errFiles, fh.Filename)
				continue
			}
			succMap[fh.Filename] = "/i/" + name
		}
	}
	writeJSON(w, http.StatusOK, shared.UploadResponse{
		Msg:  "",
		Code: 0,
		Data: shared.UploadData{ErrFiles: errFiles, SuccMap: succMap},
	})
}

// image serves an uploaded image from disk with immutable caching — names
// are random, so content never changes under the same URL.
func (s *Server) image(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.requireUser(w, r); !ok {
		return
	}
	name := r.PathValue("name")
	if !imageNameRe.MatchString(name) || s.ImageDir == "" {
		http.NotFound(w, r)
		return
	}
	mime, ok := imageExtMime[filepath.Ext(name)]
	if !ok {
		http.NotFound(w, r)
		return
	}
	b, err := os.ReadFile(filepath.Join(s.ImageDir, name))
	if err != nil {
		http.NotFound(w, r)
		return
	}
	w.Header().Set("Content-Type", mime)
	w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
	w.Header().Set("X-Cache", "HIT")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(b)
}

// serveApp delegates to the static handler: it serves real files, falls
// back to index.html for extensionless client routes, and 404s /api/* misses
// and missing assets.
func (s *Server) serveApp(w http.ResponseWriter, r *http.Request) {
	s.Static.ServeHTTP(w, r)
}
