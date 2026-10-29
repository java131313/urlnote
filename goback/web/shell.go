//go:build js && wasm

package main

import (
	"strconv"
	"strings"
	"syscall/js"

	"urlnote/goback/shared"
)

// renderLogin draws the login card. Called when showLogin flips to true.
func renderLogin() {
	root := qs("#app")
	root.Set("innerHTML", "")
	card := el("form", map[string]any{"class": "login-card"})
	card.Call("appendChild", el("strong", nil, "urlnote"))
	card.Call("appendChild", el("p", map[string]any{"class": "login-hint"}, "登录后开始编辑便签"))
	user := el("input", map[string]any{"class": "login-input", "placeholder": "用户名", "autocomplete": "username", "required": "true"})
	pass := el("input", map[string]any{"class": "login-input", "type": "password", "placeholder": "密码", "autocomplete": "current-password", "required": "true"})
	errSpan := el("span", map[string]any{"class": "login-error"})
	btn := el("button", map[string]any{"type": "submit", "class": "login-btn"}, "登 录")
	card.Call("appendChild", user)
	card.Call("appendChild", pass)
	card.Call("appendChild", btn)
	card.Call("appendChild", errSpan)
	on(card, "submit", func(e jsValue) {
		e.Call("preventDefault")
		errSpan.Set("textContent", "")
		go func() {
			var resp shared.LoginResponse
			if fetchJSON("POST", apiURL("/api/login"), shared.LoginRequest{Username: user.Get("value").String(), Password: pass.Get("value").String()}, &resp) {
				app.user = resp.User.Username
				app.showLogin = false
				renderMain()
				app.refreshEditStatus()
				app.loadNote()
			} else {
				errSpan.Set("textContent", "登录失败")
			}
		}()
	})
	wrap := el("div", map[string]any{"class": "login-wrap"}, card)
	root.Call("appendChild", wrap)
	setTimeout(0, func() { user.Call("focus") })
}

// renderMain draws the full note-editing shell (header + editor wrap).
// Dynamic regions (picker, panels, banners) re-render via their own helpers.
func renderMain() {
	root := qs("#app")
	root.Set("innerHTML", "")
	a := app

	header := el("header", nil)
	header.Call("appendChild", el("strong", nil, "urlnote"))
	header.Call("appendChild", el("a", map[string]any{"href": BASE_PATH + "/" + a.id, "title": "当前便签地址"}, BASE_PATH+"/"+a.id))
	header.Call("appendChild", el("a", map[string]any{"href": BASE_PATH + "/" + newID(), "title": "新建一个便签"}, "＋ 新建"))
	header.Call("appendChild", el("a", map[string]any{"href": BASE_PATH + "/oss", "title": "对象存储文件库"}, "🗂 文件库"))

	titleInput := el("input", map[string]any{
		"class": "title-input", "placeholder": "未命名便签", "maxlength": "200", "title": "便签标题",
	})
	header.Call("appendChild", titleInput)

	authorBadge := el("span", map[string]any{"class": "author-badge", "title": "最后编辑者", "style": "display:none"}, "✍️ ")
	header.Call("appendChild", authorBadge)

	presenceBadge := el("span", map[string]any{"class": "presence-badge", "style": "display:none"})
	header.Call("appendChild", presenceBadge)

	tagsWrap := el("span", map[string]any{"class": "tags-wrap", "title": "标签（逗号分隔，用于分类）"})
	tagsChips := el("span", nil)
	tagsInput := el("input", map[string]any{"class": "tags-input", "placeholder": "标签，逗号分隔"})
	tagsWrap.Call("appendChild", tagsChips)
	tagsWrap.Call("appendChild", tagsInput)
	header.Call("appendChild", tagsWrap)

	// ---- picker (全部便签) ----
	pickerSpan := el("span", map[string]any{"class": "picker"})
	pickerBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "全部便签"}, "☰ 便签")
	pickerPanel := el("div", map[string]any{"class": "picker-panel", "style": "display:none"})
	pickerSearch := el("input", map[string]any{"class": "picker-search", "placeholder": "搜索标题、标签或 ID…"})
	pickerList := el("ul", map[string]any{"class": "picker-list"})
	pickerPanel.Call("appendChild", pickerSearch)
	pickerPanel.Call("appendChild", pickerList)
	pickerSpan.Call("appendChild", pickerBtn)
	pickerSpan.Call("appendChild", pickerPanel)
	header.Call("appendChild", pickerSpan)
	on(pickerPanel, "click", func(e jsValue) { e.Call("stopPropagation") })

	// ---- admin tools ----
	lockBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "锁定后仅你和已批准的用户可以编辑", "style": "display:none"})
	header.Call("appendChild", lockBtn)
	reqSpan := el("span", map[string]any{"class": "req-panel", "style": "display:none"})
	reqBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "编辑权限申请"})
	reqListPanel := el("div", map[string]any{"class": "req-list-panel", "style": "display:none"})
	reqList := el("ul", map[string]any{"class": "req-list"})
	reqListPanel.Call("appendChild", reqList)
	reqSpan.Call("appendChild", reqBtn)
	reqSpan.Call("appendChild", reqListPanel)
	header.Call("appendChild", reqSpan)
	on(reqListPanel, "click", func(e jsValue) { e.Call("stopPropagation") })

	// ---- tools ----
	tools := el("span", map[string]any{"class": "tools"})
	exportBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "把编辑的内容导出为 Markdown 文件"}, "⬇ 导出")
	analyzeBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "分析中文/日文的词法问题并给出更正建议"}, "🔍 分析")
	posBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "按词性给中文/日文词语着色标注(名词/动词/形容词/副词…)"}, "🎨 词性")
	findBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "在本文便签中查找并高亮(全站匹配数来自搜索接口)"}, "🔎 查找")
	findInput := el("input", map[string]any{"class": "find-input", "placeholder": "查找…", "style": "display:none"})
	findMeta := el("span", map[string]any{"class": "find-meta", "style": "display:none"})
	findMetaAll := el("span", map[string]any{"class": "find-meta all", "style": "display:none"})
	uploadBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "导入文件（Markdown / Word / PDF），替换当前全文"}, "⬆ 上传")
	appendBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "选择一个或多个文件（Markdown / Word / PDF），依次追加到文末"}, "➕ 追加")
	fontDown := el("button", map[string]any{"type": "button", "class": "tool", "title": "缩小显示 (Ctrl+-)"}, "A−")
	fontUp := el("button", map[string]any{"type": "button", "class": "tool", "title": "放大显示 (Ctrl+=)"}, "A+")
	fontReset := el("button", map[string]any{"type": "button", "class": "tool", "title": "恢复默认大小 (Ctrl+0)", "style": "display:none"}, "A")
	colorInput := el("input", map[string]any{"type": "color", "class": "color", "title": "文字颜色", "value": "#000000"})
	colorReset := el("button", map[string]any{"type": "button", "class": "tool", "title": "恢复默认颜色", "style": "display:none"}, "默认")
	fileInput := el("input", map[string]any{"type": "file", "class": "file-hidden", "accept": ".md,.markdown,.txt,.docx,.pdf,text/markdown,text/plain,application/vnd.openxmlformats-officedocument.wordprocessingml.document,application/pdf"})
	appendInput := el("input", map[string]any{"type": "file", "class": "file-hidden", "multiple": "true", "accept": ".md,.markdown,.txt,.docx,.pdf,text/markdown,text/plain,application/vnd.openxmlformats-officedocument.wordprocessingml.document,application/pdf"})
	for _, b := range []jsValue{exportBtn, analyzeBtn, posBtn, findBtn, uploadBtn, appendBtn, fontDown, fontUp, fontReset, colorInput, colorReset} {
		tools.Call("appendChild", b)
	}
	tools.Call("appendChild", fileInput)
	tools.Call("appendChild", appendInput)
	header.Call("appendChild", tools)

	userWrap := el("span", map[string]any{"class": "user-wrap", "title": "当前用户"}, "👤 "+a.user+" ")
	logoutBtn := el("button", map[string]any{"type": "button", "class": "tool"}, "退出")
	userWrap.Call("appendChild", logoutBtn)
	header.Call("appendChild", userWrap)

	saveBtn := el("button", map[string]any{"type": "button", "class": "tool save-btn", "title": "立即保存 (Ctrl+S)"}, "💾 保存")
	header.Call("appendChild", saveBtn)
	status := el("span", map[string]any{"id": "status", "class": a.statusClass}, a.statusText)
	header.Call("appendChild", status)

	// ---- banners ----
	markBanner := el("div", map[string]any{"class": "mark-banner", "style": "display:none"},
		"📝 正在编辑标记文档 — 保存只写入 notes_mark,原便签不受影响 ")
	markBannerExit := el("button", map[string]any{"type": "button"}, "返回原文")
	markBanner.Call("appendChild", markBannerExit)
	syncBanner := el("div", map[string]any{"class": "sync-banner", "style": "display:none"}, "⚠️ 此便签已被其他人更新 ")
	syncBannerLoad := el("button", map[string]any{"type": "button"}, "载入最新内容")
	syncBanner.Call("appendChild", syncBannerLoad)

	// ---- editor wrap ----
	editorWrap := el("div", map[string]any{"class": "editor-wrap"})
	editorDiv := el("div", map[string]any{"class": "editor"})
	posOverlay := el("div", map[string]any{"class": "pos-overlay"}, el("div", map[string]any{"class": "pos-layer"}))
	posLegend := el("div", map[string]any{"class": "pos-legend", "style": "display:none"})
	editorWrap.Call("appendChild", editorDiv)
	editorWrap.Call("appendChild", posOverlay)
	editorWrap.Call("appendChild", posLegend)

	root.Call("appendChild", header)
	root.Call("appendChild", markBanner)
	root.Call("appendChild", syncBanner)
	root.Call("appendChild", editorWrap)

	// ---- wiring ----
	a.editorDiv = editorDiv
	a.titleInput = titleInput
	a.tagsInput = tagsInput
	a.tagsChips = tagsChips
	a.authorBadge = authorBadge
	a.presenceBadge = presenceBadge
	a.pickerPanel = pickerPanel
	a.pickerList = pickerList
	a.pickerBtn = pickerBtn
	a.lockBtn = lockBtn
	a.reqSpan = reqSpan
	a.reqBtn = reqBtn
	a.reqListPanel = reqListPanel
	a.reqList = reqList
	a.posBtn = posBtn
	a.findBtn = findBtn
	a.findInput = findInput
	a.findMeta = findMeta
	a.findMetaAll = findMetaAll
	a.uploadBtn = uploadBtn
	a.appendBtn = appendBtn
	a.fontReset = fontReset
	a.colorInput = colorInput
	a.colorReset = colorReset
	a.markBanner = markBanner
	a.syncBanner = syncBanner
	a.posLegend = posLegend
	a.posOverlay = posOverlay

	on(titleInput, "input", func(e jsValue) {
		a.title = titleInput.Get("value").String()
		a.watchBaseline()
	})
	on(tagsInput, "input", func(e jsValue) {
		a.tagsText = tagsInput.Get("value").String()
		renderTags()
		a.watchBaseline()
	})
	on(pickerBtn, "click", func(e jsValue) {
		e.Call("stopPropagation")
		togglePicker()
	})
	on(pickerSearch, "input", func(e jsValue) {
		a.onPickerQuery(pickerSearch.Get("value").String())
	})
	on(lockBtn, "click", func(e jsValue) { a.toggleEditLock() })
	on(reqBtn, "click", func(e jsValue) {
		e.Call("stopPropagation")
		a.reqPanelOpen = !a.reqPanelOpen
		renderReqPanel()
		if a.reqPanelOpen {
			a.loadEditRequests()
		}
	})
	on(exportBtn, "click", func(e jsValue) { a.exportMD() })
	on(analyzeBtn, "click", func(e jsValue) { a.runAnalysis() })
	on(posBtn, "click", func(e jsValue) { a.togglePos() })
	on(findBtn, "click", func(e jsValue) { a.toggleFind() })
	on(findInput, "input", func(e jsValue) { a.onFindQuery(findInput.Get("value").String()) })
	on(findInput, "keydown", func(e jsValue) {
		if e.Get("key").String() == "Escape" {
			a.toggleFind()
		}
	})
	on(uploadBtn, "click", func(e jsValue) { fileInput.Call("click") })
	on(appendBtn, "click", func(e jsValue) { appendInput.Call("click") })
	on(fileInput, "change", func(e jsValue) { a.readFilesIntoEditor(false, fileInput) })
	on(appendInput, "change", func(e jsValue) { a.readFilesIntoEditor(true, appendInput) })
	on(fontDown, "click", func(e jsValue) { a.bumpFont(-fontStep) })
	on(fontUp, "click", func(e jsValue) { a.bumpFont(fontStep) })
	on(fontReset, "click", func(e jsValue) { a.setFontSize(shared.DefaultFontSize) })
	on(colorInput, "input", func(e jsValue) {
		a.color = colorInput.Get("value").String()
		a.applyEditorStyle()
		a.watchBaseline()
	})
	on(colorReset, "click", func(e jsValue) {
		a.color = ""
		a.applyEditorStyle()
		renderColorReset()
		a.watchBaseline()
	})
	on(logoutBtn, "click", func(e jsValue) {
		go func() {
			fetchStatus("POST", apiURL("/api/logout"), nil)
			js.Global().Get("location").Call("reload")
		}()
	})
	on(saveBtn, "click", func(e jsValue) { a.saveNow() })
	on(markBannerExit, "click", func(e jsValue) { a.exitMarkMode() })
	on(syncBannerLoad, "click", func(e jsValue) { a.loadLatest() })

	// document-level listeners (keydown shortcuts, outside-click closing,
	// suggestion tracking) — mirrors the Rust Effect/listener setup.
	on(js.Global(), "keydown", func(e jsValue) {
		if !(e.Get("ctrlKey").Bool() || e.Get("metaKey").Bool()) {
			return
		}
		switch e.Get("key").String() {
		case "s", "S":
			e.Call("preventDefault")
			a.saveNow()
		case "=", "+":
			e.Call("preventDefault")
			a.bumpFont(fontStep)
		case "-":
			e.Call("preventDefault")
			a.bumpFont(-fontStep)
		case "0":
			e.Call("preventDefault")
			a.setFontSize(shared.DefaultFontSize)
		}
	})
	on(doc(), "click", func(e jsValue) {
		t := e.Get("target")
		if !t.Truthy() {
			return
		}
		el := t
		inside := false
		insideReq := false
		if el.Get("closest").Type() == js.TypeFunction {
			inside = el.Call("closest", ".picker, .req-panel").Truthy()
		}
		if !inside && a.pickerOpen {
			a.pickerOpen = false
			renderPicker()
		}
		if !inside && a.reqPanelOpen {
			a.reqPanelOpen = false
			renderReqPanel()
		}
		_ = insideReq
	})
	on(doc(), "selectionchange", func(e jsValue) { a.updateSuggestions() })
	onCapture(doc(), "keydown", func(e jsValue) {
		key := e.Get("key").String()
		switch key {
		case "ArrowDown", "ArrowUp", "Enter", "Tab", "Escape":
		default:
			return
		}
		if len(a.suggestItems) == 0 {
			return
		}
		e.Call("preventDefault")
		e.Call("stopPropagation")
		a.onSuggestKeydown(key)
	})
	// keep the editor theme in sync with the OS appearance
	mql := js.Global().Call("matchMedia", "(prefers-color-scheme: dark)")
	if mql.Truthy() {
		on(mql, "change", func(e jsValue) {
			matches := e.Get("matches").Bool()
			if matches {
				a.editor.setTheme("dark")
			} else {
				a.editor.setTheme("classic")
			}
		})
	}

	// initial state-dependent renders
	renderTitle()
	renderTags()
	renderFindBar()
	renderAdminTools()
	renderLockBanner()
	renderPosLegend()
}

const fontStep = 2

// ---- small render helpers (the Leptos <Show>/<For> blocks) ----

func (a *App) applyEditorStyle() {
	editor := qs(".editor")
	if !editor.Truthy() {
		return
	}
	s := "--note-zoom: " + strconv.FormatFloat(float64(a.fontSize)/float64(shared.DefaultFontSize), 'f', 3, 64) + ";"
	if a.color != "" {
		s += " --note-color: " + a.color + ";"
	}
	editor.Set("style", s)
}

func renderTitle() {
	if app.titleInput.Truthy() {
		app.titleInput.Set("value", app.title)
	}
}

func renderTags() {
	if app.tagsChips.Truthy() {
		html := ""
		for _, t := range app.parsedTags() {
			html += `<span class="chip">` + escapeHTML(t) + `</span>`
		}
		app.tagsChips.Set("innerHTML", html)
	}
}

func renderAuthor() {
	b := app.authorBadge
	if !b.Truthy() {
		return
	}
	if app.author == "" {
		b.Set("style", "display:none")
	} else {
		b.Set("style", "")
		b.Set("textContent", "✍️ "+app.author)
	}
}

func renderPresence() {
	b := app.presenceBadge
	if !b.Truthy() {
		return
	}
	if len(app.presenceOthers) == 0 {
		b.Set("style", "display:none")
	} else {
		b.Set("style", "")
		b.Set("textContent", "👀 "+strings.Join(app.presenceOthers, "、")+" 正在查看")
		b.Call("setAttribute", "title", "正在查看本文："+strings.Join(app.presenceOthers, "、"))
	}
}

func renderColorReset() {
	if app.colorReset.Truthy() {
		if app.color == "" {
			app.colorReset.Set("style", "display:none")
			app.colorInput.Set("value", "#000000")
		} else {
			app.colorReset.Set("style", "")
			app.colorInput.Set("value", app.color)
		}
	}
}

func renderFontReset() {
	if app.fontReset.Truthy() {
		if app.fontSize != shared.DefaultFontSize {
			app.fontReset.Set("style", "")
		} else {
			app.fontReset.Set("style", "display:none")
		}
	}
}

func renderBanners() {
	if app.markBanner.Truthy() {
		if app.markMode {
			app.markBanner.Set("style", "")
		} else {
			app.markBanner.Set("style", "display:none")
		}
	}
	if app.syncBanner.Truthy() {
		if app.remoteChanged {
			app.syncBanner.Set("style", "")
		} else {
			app.syncBanner.Set("style", "display:none")
		}
	}
}

// escapeHTML minimal HTML-escaping for text interpolated into innerHTML.
func escapeHTML(s string) string {
	r := strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;")
	return r.Replace(s)
}
