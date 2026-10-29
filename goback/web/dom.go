// Package main is the urlnote Go WASM frontend: a syscall/js port of the
// Rust/Leptos app (rust/frontend/src). The Leptos signal layer maps onto
// explicit render functions; everything else (Vditor interop, overlay
// geometry, timers, fetch) maps onto small syscall/js helpers collected
// here.
package main

import (
	"encoding/json"
	"fmt"
	"syscall/js"
)

// retained keeps js.Func callbacks alive: js.Func values handed to JS can be
// GC'd unless Go keeps a reference (the Rust code kept equivalent Closures).
var retained []js.Func

func keep(f js.Func) js.Func {
	retained = append(retained, f)
	return f
}

func win() js.Value   { return js.Global() }
func doc() js.Value   { return js.Global().Get("document") }
func body() js.Value  { return doc().Get("body") }
func nowMs() float64  { return js.Global().Get("Date").New().Call("getTime").Float() }

// qs is document.querySelector, returns undefined when not found.
func qs(sel string) js.Value { return doc().Call("querySelector", sel) }

func qsa(sel string) []js.Value {
	list := doc().Call("querySelectorAll", sel)
	out := make([]js.Value, 0, list.Length())
	for i := 0; i < list.Length(); i++ {
		out = append(out, list.Index(i))
	}
	return out
}

// el creates an element with a map of attributes (string values, "class" and
// "text" special-cased) and child nodes/values (strings become text nodes).
func el(tag string, attrs map[string]any, children ...any) js.Value {
	e := doc().Call("createElement", tag)
	for k, v := range attrs {
		switch k {
		case "text":
			e.Set("textContent", v)
		case "class":
			e.Set("className", v)
		case "value":
			e.Set("value", v)
		case "html":
			e.Set("innerHTML", v)
		default:
			e.Call("setAttribute", k, fmt.Sprint(v))
		}
	}
	for _, c := range children {
		switch v := c.(type) {
		case nil:
		case string:
			e.Call("appendChild", doc().Call("createTextNode", v))
		case js.Value:
			if v.Type() != js.TypeUndefined && v.Type() != js.TypeNull {
				e.Call("appendChild", v)
			}
		}
	}
	return e
}

// on binds an event listener, keeping the js.Func alive.
func on(target js.Value, evt string, cb func(js.Value)) {
	f := js.FuncOf(func(_ js.Value, args []js.Value) any {
		cb(args[0])
		return nil
	})
	keep(f)
	target.Call("addEventListener", evt, f)
}

// onCapture binds a capturing listener (scroll/keydown semantics like the
// Rust run_in_capture_phase usages).
func onCapture(target js.Value, evt string, cb func(js.Value)) {
	f := js.FuncOf(func(_ js.Value, args []js.Value) any {
		cb(args[0])
		return nil
	})
	keep(f)
	target.Call("addEventListener", evt, f, true)
}

// setTimeout schedules fn after ms milliseconds; returns the js.Func (for
// debounce cancellation use clearTimeout with the returned id).
func setTimeout(ms int, fn func()) js.Value {
	f := js.FuncOf(func(_ js.Value, _ []js.Value) any {
		fn()
		return nil
	})
	keep(f)
	return js.Global().Call("setTimeout", f, ms)
}

func clearTimeout(id js.Value) { js.Global().Call("clearTimeout", id) }

func setInterval(ms int, fn func()) js.Value {
	f := js.FuncOf(func(_ js.Value, _ []js.Value) any {
		fn()
		return nil
	})
	keep(f)
	return js.Global().Call("setInterval", f, ms)
}

// ---- fetch (credentials: include, JSON conveniences) ----

func await(p js.Value) js.Value {
	ch := make(chan js.Value)
	resolve := js.FuncOf(func(_ js.Value, args []js.Value) any {
		ch <- args[0]
		return nil
	})
	keep(resolve)
	p.Call("then", resolve)
	return <-ch
}

// fetchDo performs a fetch with credentials:"include" and returns the
// Response (check Get("ok"), Get("status")).
func fetchDo(method, url string, body any, headers map[string]string) js.Value {
	opts := js.Global().Get("Object").New()
	opts.Set("method", method)
	opts.Set("credentials", "include")
	if body != nil {
		opts.Set("body", body)
	}
	if len(headers) > 0 {
		h := js.Global().Get("Object").New()
		for k, v := range headers {
			h.Set(k, v)
		}
		opts.Set("headers", h)
	}
	return await(js.Global().Call("fetch", url, opts))
}

// fetchJSON GETs url and unmarshals the JSON body into out. Returns false on
// any failure (network, non-2xx, bad JSON) — handlers degrade silently like
// the Rust code.
func fetchJSON(method, url string, body any, out any) bool {
	var jsBody js.Value
	headers := map[string]string{}
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return false
		}
		jsBody = js.ValueOf(string(b))
		headers["Content-Type"] = "application/json"
	}
	resp := fetchDo(method, url, jsBody, headers)
	if !resp.Get("ok").Bool() {
		return false
	}
	jsonVal := await(resp.Call("json"))
	text := js.Global().Get("JSON").Call("stringify", jsonVal).String()
	return json.Unmarshal([]byte(text), out) == nil
}

// fetchStatus performs a request and returns (status, ok); body may be nil.
func fetchStatus(method, url string, body any) (int, bool) {
	var jsBody js.Value
	headers := map[string]string{}
	if body != nil {
		b, _ := json.Marshal(body)
		jsBody = js.ValueOf(string(b))
		headers["Content-Type"] = "application/json"
	}
	resp := fetchDo(method, url, jsBody, headers)
	return resp.Get("status").Int(), resp.Get("ok").Bool()
}

// ---- misc browser helpers ----

func confirm(msg string) bool {
	return js.Global().Call("confirm", msg).Bool()
}

func prompt(msg string) (string, bool) {
	v := js.Global().Call("prompt", msg)
	if v.IsNull() {
		return "", false
	}
	return v.String(), true
}

func currentTime() string {
	s := js.Global().Get("Date").New().Call("toTimeString").String()
	if len(s) >= 8 {
		return s[:8]
	}
	return s
}

func downloadText(filename, content string) bool {
	arr := js.Global().Get("Array").New()
	arr.Call("push", content)
	blob := js.Global().Get("Blob").New(arr)
	url := js.Global().Get("URL").Call("createObjectURL", blob)
	a := el("a", nil)
	a.Set("href", url)
	a.Set("download", filename)
	a.Call("click")
	js.Global().Get("URL").Call("revokeObjectURL", url)
	return true
}

// encodeURIComponent wraps the JS encoder for query strings.
func encodeURIComponent(s string) string {
	return js.Global().Call("encodeURIComponent", s).String()
}
