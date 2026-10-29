//go:build js && wasm

package main

import (
	"encoding/base64"
	"encoding/json"
	"strings"
	"syscall/js"

	"urlnote/goback/shared"
)

// ---- load & editor ----

func (a *App) loadNote() {
	if a.hasEdit {
		return
	}
	go func() {
		resp := fetchDo("GET", apiURL("/api/note/"+a.id), nil, nil)
		if resp.Get("status").Int() == 401 {
			a.showLogin = true
			renderLogin()
			return
		}
		if !resp.Get("ok").Bool() {
			a.setStatus("加载失败", "error")
			return
		}
		var d shared.Note
		j := await(resp.Call("json"))
		if json.Unmarshal([]byte(js.Global().Get("JSON").Call("stringify", j).String()), &d) != nil {
			a.setStatus("加载失败", "error")
			return
		}
		a.fontSize = shared.ResolveFontSize(float64(d.FontSize))
		a.applyEditorStyle()
		a.color = d.Color
		renderColorReset()
		a.applyEditorStyle()
		a.title = d.Title
		renderTitle()
		a.tagsText = strings.Join(d.Tags, ", ")
		if a.tagsInput.Truthy() {
			a.tagsInput.Set("value", a.tagsText)
		}
		renderTags()
		a.author = d.Author
		renderAuthor()
		a.pollUpdatedAt = d.UpdatedAt
		if d.Content == "" {
			a.setStatus("", "")
		} else {
			a.setStatus("已加载", "")
		}
		a.loaded = true
		a.suppressInputUntil = nowMs() + 600
		a.lastApplied = d.Content
		a.baselineFont = shared.ResolveFontSize(float64(d.FontSize))
		a.baselineColor = d.Color
		a.baselineTitle = d.Title
		a.baselineTags = strings.Join(d.Tags, ", ")
		dark := js.Global().Call("matchMedia", "(prefers-color-scheme: dark)").Get("matches").Bool()
		a.initEditor(d.Content, dark)
		a.startCollaboration()
	}()
}

func (a *App) initEditor(markdown string, dark bool) {
	mount := qs(".editor")
	if !mount.Truthy() {
		return
	}
	opts := js.Global().Get("Object").New()
	setOpt := func(k string, v any) { opts.Set(k, v) }
	setOpt("value", markdown)
	setOpt("mode", "wysiwyg")
	setOpt("lang", "zh_CN")
	// serve Vditor's runtime assets from our own static assets
	setOpt("cdn", BASE_PATH+"/vditor")
	if dark {
		setOpt("theme", "dark")
	} else {
		setOpt("theme", "classic")
	}
	preview := js.Global().Get("Object").New()
	if dark {
		preview.Set("theme", "dark")
	} else {
		preview.Set("theme", "light")
	}
	setOpt("preview", preview)
	setOpt("placeholder", "在这里输入 Markdown，内容会自动保存到这个网址…")
	cache := js.Global().Get("Object").New()
	cache.Set("enable", false)
	setOpt("cache", cache)

	// toolbar image button / paste / drag-drop upload to the server. A
	// custom handler instead of Vditor's built-in `url` XHR upload: the XHR
	// is sent without credentials; fetch sends the cookie.
	upload := js.Global().Get("Object").New()
	upload.Set("max", 8*1024*1024)
	upload.Set("fieldName", "file[]")
	uploadCb := js.FuncOf(func(_ js.Value, args []js.Value) any {
		files := args[0]
		go a.handleEditorUpload(files)
		return nil
	})
	keep(uploadCb)
	upload.Set("handler", uploadCb)
	setOpt("upload", upload)
	a.editor.cbs = append(a.editor.cbs, uploadCb)

	toolbar := js.Global().Get("Array").New()
	for _, t := range []string{
		"headings", "bold", "italic", "strike", "|",
		"list", "ordered-list", "check", "quote", "line", "code", "inline-code", "link",
		"upload", "table", "|", "undo", "redo", "|", "edit-mode", "preview",
	} {
		toolbar.Call("push", t)
	}
	setOpt("toolbar", toolbar)

	inputCb := js.FuncOf(func(_ js.Value, _ []js.Value) any {
		a.onEditorInput()
		return nil
	})
	keep(inputCb)
	setOpt("input", inputCb)
	a.editor.cbs = append(a.editor.cbs, inputCb)

	blurCb := js.FuncOf(func(_ js.Value, _ []js.Value) any {
		// defer so a click on a suggestion row still lands first
		setTimeout(150, func() { a.closeSuggestions() })
		return nil
	})
	keep(blurCb)
	setOpt("blur", blurCb)
	a.editor.cbs = append(a.editor.cbs, blurCb)

	afterCb := js.FuncOf(func(_ js.Value, _ []js.Value) any {
		a.editor.focus()
		return nil
	})
	keep(afterCb)
	setOpt("after", afterCb)
	a.editor.cbs = append(a.editor.cbs, afterCb)

	a.editor.v = js.Global().Get("Vditor").New(mount, opts)
	a.hasEdit = true
}

func (a *App) onEditorInput() {
	if a.applyingRemote || nowMs() < a.suppressInputUntil {
		return
	}
	// an input event whose value equals the last applied content is an echo
	if strings.TrimRight(storageMD(a.editor.getValue()), " \t\r\n") == strings.TrimRight(a.lastApplied, " \t\r\n") {
		return
	}
	a.markDirty()
	a.updateSuggestions()
	a.schedulePosRefresh(false)
	if a.findOpen && strings.TrimSpace(a.findQuery) != "" {
		if a.findDebounce.Truthy() {
			clearTimeout(a.findDebounce)
		}
		a.findDebounce = setTimeout(400, func() { a.runFind() })
	}
}

func (a *App) handleEditorUpload(files js.Value) {
	form := js.Global().Get("FormData").New()
	arr := js.Global().Get("Array").Call("from", files)
	for i := 0; i < arr.Length(); i++ {
		form.Call("append", "file[]", arr.Index(i))
	}
	resp := fetchDo("POST", apiURL("/api/upload"), form, nil)
	if !resp.Get("ok").Bool() {
		a.setStatus("图片上传失败", "error")
		return
	}
	var ur shared.UploadResponse
	j := await(resp.Call("json"))
	if json.Unmarshal([]byte(js.Global().Get("JSON").Call("stringify", j).String()), &ur) != nil {
		a.setStatus("图片上传失败", "error")
		return
	}
	for name, path := range ur.Data.SuccMap {
		a.editor.insertValue("!["+name+"]("+apiURL(path)+")\n", true)
	}
	if len(ur.Data.ErrFiles) > 0 {
		a.setStatus("部分图片上传失败", "error")
	}
}

func (a *App) startCollaboration() {
	a.beatPresence()
	a.presenceTimer = setInterval(15000, func() { a.beatPresence() })
	a.pollTimer = setInterval(10000, func() { a.pollNote() })
	// keep lock/request state fresh
	a.lockTimer = setInterval(15000, func() { a.refreshEditStatus() })
	// autosave flushes pending edits every 3 minutes
	a.autosaveTimer = setInterval(180000, func() {
		if a.dirty && a.hasEdit {
			a.save()
		}
	})
}

// ---- export & import ----

func (a *App) exportMD() {
	content := a.editor.getValue()
	if strings.TrimSpace(content) == "" {
		a.setStatus("没有内容可导出", "error")
		return
	}
	name := sanitizeFilename(a.title)
	if name == "" {
		name = a.id
	}
	if downloadText(name+".md", content) {
		a.setStatus("已导出 "+name+".md", "saved")
	} else {
		a.setStatus("导出失败", "error")
	}
}

func sanitizeFilename(t string) string {
	var b strings.Builder
	n := 0
	for _, c := range t {
		switch c {
		case '/', '\\', ':', '*', '?', '"', '<', '>', '|', '\n', '\r':
			continue
		}
		b.WriteRune(c)
		n++
		if n >= 60 {
			break
		}
	}
	return strings.TrimSpace(b.String())
}

// readFilesIntoEditor loads the selected files (replace or append),
// converting docx/pdf via the browser-side libraries.
func (a *App) readFilesIntoEditor(appendMode bool, input js.Value) {
	if !a.hasEdit {
		a.setStatus("编辑器尚未加载完成", "error")
		return
	}
	files := input.Get("files")
	if !files.Truthy() || files.Get("length").Int() == 0 {
		return
	}
	if !appendMode {
		current := a.editor.getValue()
		if strings.TrimSpace(current) != "" &&
			!confirm("当前编辑已有内容，确定要替换为文件内容吗？") {
			input.Set("value", "")
			return
		}
	}
	// Snapshot the File handles before clearing the input (FileList is live).
	var picked []js.Value
	for i := 0; i < files.Get("length").Int(); i++ {
		picked = append(picked, files.Call("item", i))
	}
	input.Set("value", "")
	if len(picked) == 0 {
		return
	}
	go func() {
		content := ""
		if appendMode {
			content = a.editor.getValue()
		}
		names := []string{}
		converted := 0
		for _, file := range picked {
			name := file.Get("name").String()
			names = append(names, name)
			lower := strings.ToLower(name)
			var text string
			var err error
			switch {
			case strings.HasSuffix(lower, ".docx"):
				text, err = docxToMarkdown(file)
				if err == nil {
					converted++
				}
			case strings.HasSuffix(lower, ".pdf"):
				text, err = pdfToText(file)
				if err == nil {
					converted++
				}
			default:
				p := await(file.Call("text"))
				text = p.String()
				err = nil
			}
			if err != nil {
				a.setStatus(name+"："+err.Error(), "error")
				continue
			}
			if appendMode {
				if content != "" {
					content += "\n\n"
				}
				content += text
			} else {
				content = text
			}
		}
		a.editor.setValue(content)
		a.markDirty()
		if converted > 0 {
			a.setStatus("已载入 "+itoa(len(names))+" 个文件（转换 "+itoa(converted)+" 个），正在保存…", "saving")
		} else {
			a.setStatus("已载入 "+itoa(len(names))+" 个文件，正在保存…", "saving")
		}
		a.save()
	}()
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	var digits []byte
	for n > 0 {
		digits = append([]byte{byte('0' + n%10)}, digits...)
		n /= 10
	}
	return string(digits)
}

// jsFn fetches a function property, erroring like the Rust js_fn helper.
func jsFn(obj js.Value, name string) (js.Value, error) {
	f := obj.Get(name)
	if !f.Truthy() || f.Type() != js.TypeFunction {
		return js.Undefined(), errString("JS 函数缺失: " + name)
	}
	return f, nil
}

type errString string

func (e errString) Error() string { return string(e) }

// jsCallPromise calls an object's method returning a Promise and awaits it.
func jsCallPromise(this js.Value, name string, arg js.Value) (js.Value, error) {
	f, err := jsFn(this, name)
	if err != nil {
		return js.Undefined(), err
	}
	var p js.Value
	if arg.Type() != js.TypeUndefined {
		p = f.Call("call", this, arg)
	} else {
		p = f.Call("call", this)
	}
	if p.Type() != js.TypeObject || p.Get("then").Type() != js.TypeFunction {
		return js.Undefined(), errString(name + " 未返回 Promise")
	}
	v := await(p)
	if js.Global().Get("JSON").Call("stringify", v).String() == "null" && v.IsNull() {
		return v, nil
	}
	return v, nil
}

// docxToMarkdown converts .docx → HTML (mammoth) → markdown (Turndown),
// uploading embedded data-URI images through /api/upload.
func docxToMarkdown(file js.Value) (string, error) {
	buf := await(file.Call("arrayBuffer"))
	mammoth := js.Global().Get("mammoth")
	if !mammoth.Truthy() {
		return "", errString("docx 转换库未加载")
	}
	opts := js.Global().Get("Object").New()
	opts.Set("arrayBuffer", buf)
	res, err := jsCallPromise(mammoth, "convertToHtml", opts)
	if err != nil {
		return "", err
	}
	html := res.Get("value").String()

	td, err := jsFn(js.Global(), "TurndownService")
	if err != nil {
		return "", errString("Markdown 转换库未加载")
	}
	topts := js.Global().Get("Object").New()
	topts.Set("headingStyle", "atx")
	topts.Set("hr", "---")
	topts.Set("bulletListMarker", "-")
	topts.Set("codeBlockStyle", "fenced")
	topts.Set("emDelimiter", "*")
	args := js.Global().Get("Array").New(topts)
	svc := js.Global().Get("Reflect").Call("construct", td, args)
	turndown, err := jsFn(svc, "turndown")
	if err != nil {
		return "", errString("TurndownService 初始化失败")
	}
	md := turndown.Call("call", svc, html).String()
	return aUploadDataURIImages(app, md), nil
}

// uploadDataURIImages uploads every ![](data:image/...) in converted
// markdown and replaces it with an /i/ link.
func aUploadDataURIImages(a *App, md string) string {
	var out strings.Builder
	rest := md
	for {
		i := strings.Index(rest, "](data:image/")
		if i < 0 {
			break
		}
		uriStart := i + 2
		rel := strings.Index(rest[uriStart:], ")")
		if rel < 0 {
			break
		}
		uriEnd := uriStart + rel
		uri := rest[uriStart:uriEnd]
		out.WriteString(rest[:uriStart])
		if url, ok := uploadDataURI(uri); ok {
			out.WriteString(url)
		} else {
			out.WriteString(uri)
		}
		rest = rest[uriEnd:]
	}
	out.WriteString(rest)
	return out.String()
}

func uploadDataURI(uri string) (string, bool) {
	comma := strings.Index(uri, ",")
	if comma < 0 {
		return "", false
	}
	meta := uri[:comma]
	b64 := uri[comma+1:]
	if !strings.HasPrefix(meta, "data:") {
		return "", false
	}
	mime := strings.SplitN(meta[5:], ";", 2)[0]
	bytes, err := base64Decode(b64)
	if err != nil {
		return "", false
	}
	u8 := js.Global().Get("Uint8Array").New(len(bytes))
	js.Global().Get("Uint8Array").Call("from", u8, func() {}) // noop guard
	for i, b := range bytes {
		u8.SetIndex(i, b)
	}
	parts := js.Global().Get("Array").New(u8)
	blobOpts := js.Global().Get("Object").New()
	blobOpts.Set("type", mime)
	blob := js.Global().Get("Blob").New(parts, blobOpts)
	form := js.Global().Get("FormData").New()
	form.Call("append", "file[]", blob)
	resp := fetchDo("POST", apiURL("/api/upload"), form, nil)
	if !resp.Get("ok").Bool() {
		return "", false
	}
	var ur shared.UploadResponse
	j := await(resp.Call("json"))
	if json.Unmarshal([]byte(js.Global().Get("JSON").Call("stringify", j).String()), &ur) != nil {
		return "", false
	}
	for _, p := range ur.Data.SuccMap {
		return apiURL(p), true
	}
	return "", false
}

// pdfToText extracts text from a PDF via pdf.js.
func pdfToText(file js.Value) (string, error) {
	buf := await(file.Call("arrayBuffer"))
	pdfjs := js.Global().Get("pdfjsLib")
	if !pdfjs.Truthy() {
		return "", errString("PDF 转换库未加载")
	}
	if gw := pdfjs.Get("GlobalWorkerOptions"); gw.Truthy() {
		gw.Set("workerSrc", BASE_PATH+"/pdfjs/pdf.worker.min.js")
	}
	opts := js.Global().Get("Object").New()
	opts.Set("data", js.Global().Get("Uint8Array").New(buf))
	getDoc, err := jsFn(pdfjs, "getDocument")
	if err != nil {
		return "", err
	}
	task := getDoc.Call("call", pdfjs, opts)
	doc := await(task)
	numPages := int(doc.Get("numPages").Float())
	var out strings.Builder
	for p := 1; p <= numPages; p++ {
		page, err := jsCallPromise(doc, "getPage", js.ValueOf(p))
		if err != nil {
			return "", err
		}
		tc, err := jsCallPromise(page, "getTextContent", js.Undefined())
		if err != nil {
			return "", err
		}
		items := tc.Get("items")
		var line strings.Builder
		for i := 0; i < items.Length(); i++ {
			item := items.Index(i)
			line.WriteString(item.Get("str").String())
			if item.Get("hasEOL").Bool() {
				out.WriteString(strings.TrimRight(line.String(), " \t"))
				out.WriteByte('\n')
				line.Reset()
			}
		}
		out.WriteString(strings.TrimRight(line.String(), " \t"))
		out.WriteString("\n\n")
	}
	return strings.TrimRight(out.String(), " \t\r\n"), nil
}

// base64Decode decodes standard base64 (stdlib).
func base64Decode(s string) ([]byte, error) {
	return base64.StdEncoding.DecodeString(s)
}
