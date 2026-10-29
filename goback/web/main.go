//go:build js && wasm

package main

import (
	"syscall/js"

	"urlnote/goback/shared"
)

// firstPathSegment returns the first URL path segment after BASE_PATH
// without side effects (route checks must not rewrite history).
func firstPathSegment() string {
	raw := js.Global().Get("location").Get("pathname").String()
	path := raw
	if rest, ok := stringCutPrefix(path, BASE_PATH); ok && (rest == "" || rest[0] == '/') {
		path = rest
	}
	path = trimSlashPrefix(path)
	if i := indexByte(path, '/'); i >= 0 {
		return path[:i]
	}
	return path
}

// initNoteID reads the note id from the first path segment, or mints a new
// one and rewrites the URL (history.replaceState, like the Rust app).
func initNoteID() string {
	loc := js.Global().Get("location")
	raw := loc.Get("pathname").String()
	path := raw
	if rest, ok := stringCutPrefix(path, BASE_PATH); ok && (rest == "" || rest[0] == '/') {
		path = rest
	}
	path = trimSlashPrefix(path)
	id := path
	if i := indexByte(path, '/'); i >= 0 {
		id = path[:i]
	}
	if !validNoteID(id) {
		id = newID()
		js.Global().Get("history").Call("replaceState", nil, "", BASE_PATH+"/"+id)
	}
	doc().Set("title", id+" · urlnote")
	return id
}

// validNoteID mirrors shared validation: ^[a-zA-Z0-9_-]{1,64}$.
func validNoteID(id string) bool {
	if id == "" || len(id) > 64 {
		return false
	}
	for _, b := range []byte(id) {
		if !(b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z' || b >= '0' && b <= '9' || b == '_' || b == '-') {
			return false
		}
	}
	return true
}

func stringCutPrefix(s, prefix string) (string, bool) {
	if len(s) >= len(prefix) && s[:len(prefix)] == prefix {
		return s[len(prefix):], true
	}
	return s, false
}

func trimSlashPrefix(s string) string {
	for len(s) > 0 && s[0] == '/' {
		s = s[1:]
	}
	return s
}

func indexByte(s string, b byte) int {
	for i := 0; i < len(s); i++ {
		if s[i] == b {
			return i
		}
	}
	return -1
}

func main() {
	// keep the Go runtime alive: the WASM module must not return from main
	c := make(chan struct{})

	if firstPathSegment() == "oss" {
		renderOssPage()
		if !ossEnabled() {
			// not configured: show the hint instead of an empty page
			body := qs(".oss-body")
			if body.Truthy() {
				body.Set("innerHTML", "")
				body.Call("appendChild", el("div", map[string]any{"class": "oss-empty"}, "对象存储未配置：需要 OSS_AK / OSS_BUCKET 构建参数"))
			}
		}
		<-c
		return
	}

	preloadSuggestDicts()

	id := initNoteID()
	app = newApp(id)
	renderMain()

	// whoami on mount → load the note, or show the login form
	go func() {
		var d shared.WhoamiResponse
		if fetchJSON("GET", apiURL("/api/whoami"), nil, &d) {
			app.user = d.User.Username
			app.refreshEditStatus()
			app.loadNote()
		} else {
			app.showLogin = true
			renderLogin()
		}
	}()

	<-c
}
