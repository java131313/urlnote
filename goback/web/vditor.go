//go:build js && wasm

package main

import "syscall/js"

// Small aliases so App struct fields read cleanly.
type jsValue = js.Value

func jsUndefined() js.Value { return js.Undefined() }

// jsEditor wraps the window.Vditor instance plus the callbacks that must
// stay alive for the editor's lifetime (the Rust VditorHandle kept _cbs).
type jsEditor struct {
	v    js.Value // undefined until the editor is constructed
	cbs  []js.Func
	dead bool
}

func (e *jsEditor) alive() bool { return e.v.Type() != js.TypeUndefined && !e.dead }

func (e *jsEditor) getValue() string {
	if !e.alive() {
		return ""
	}
	return e.v.Call("getValue").String()
}

func (e *jsEditor) setValue(s string) {
	if e.alive() {
		e.v.Call("setValue", s)
	}
}

func (e *jsEditor) insertValue(s string, render bool) {
	if e.alive() {
		e.v.Call("insertValue", s, render)
	}
}

func (e *jsEditor) deleteValue() {
	if e.alive() {
		e.v.Call("deleteValue")
	}
}

func (e *jsEditor) focus() {
	if e.alive() {
		e.v.Call("focus")
	}
}

func (e *jsEditor) setTheme(theme string) {
	if e.alive() {
		e.v.Call("setTheme", theme)
	}
}

func (e *jsEditor) destroy() {
	if e.alive() {
		e.v.Call("destroy")
	}
	e.dead = true
}
