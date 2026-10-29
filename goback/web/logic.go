//go:build js && wasm

package main

import (
	"encoding/json"
	"strconv"
	"strings"
	"syscall/js"

	"urlnote/goback/shared"
)

const fontStepSize = 2

// newID generates a 5-char random note id (same alphabet as the Rust app).
func newID() string {
	const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	buf := js.Global().Get("Uint8Array").New(5)
	js.Global().Get("crypto").Call("getRandomValues", buf)
	out := make([]byte, 5)
	for i := 0; i < 5; i++ {
		out[i] = alphabet[int(buf.Index(i).Int())%len(alphabet)]
	}
	return string(out)
}

// ---- edit lock ----

func (a *App) refreshEditStatus() {
	go func() {
		var d shared.EditStatusResponse
		if fetchJSON("GET", apiURL("/api/edit-status"), nil, &d) {
			a.isAdmin = d.IsAdmin
			a.editLocked = d.Locked
			a.canEdit = d.CanEdit
			a.editRequest = d.Request
			renderAdminTools()
			renderLockBanner()
			updateEditability()
		}
	}()
}

func (a *App) toggleEditLock() {
	locked := !a.editLocked
	go func() {
		if _, ok := fetchStatus("POST", apiURL("/api/edit-lock"), shared.EditLockRequest{Locked: locked}); ok {
			a.refreshEditStatus()
			if locked {
				a.setStatus("已锁定编辑", "saved")
			} else {
				a.setStatus("已解锁编辑", "saved")
			}
		} else {
			a.setStatus("锁定操作失败", "error")
		}
	}()
}

func (a *App) submitEditRequest() {
	msg := a.reqMessage
	go func() {
		if _, ok := fetchStatus("POST", apiURL("/api/edit-request"), shared.EditRequestSubmit{Message: msg}); ok {
			a.reqMessage = ""
			a.refreshEditStatus()
			a.setStatus("已提交编辑申请，等待管理员审批", "saved")
		} else {
			a.setStatus("申请提交失败", "error")
		}
	}()
}

func (a *App) loadEditRequests() {
	go func() {
		var d shared.EditRequestList
		if fetchJSON("GET", apiURL("/api/edit-requests"), nil, &d) {
			a.editRequests = d.Requests
			renderReqPanel()
			renderAdminTools()
		}
	}()
}

func (a *App) handleEditRequest(username, action string) {
	go func() {
		if _, ok := fetchStatus("POST", apiURL("/api/edit-requests/handle"), shared.EditHandleRequest{Username: username, Action: action}); ok {
			a.loadEditRequests()
			a.refreshEditStatus()
			switch action {
			case "approve":
				a.setStatus("已批准 "+username+" 的编辑权限", "saved")
			case "reject":
				a.setStatus("已拒绝 "+username+" 的申请", "saved")
			default:
				a.setStatus("已撤销 "+username+" 的编辑权限", "saved")
			}
		} else {
			a.setStatus("操作失败", "error")
		}
	}()
}

// ---- picker (全部便签) ----

func togglePicker() {
	a := app
	a.pickerOpen = !a.pickerOpen
	renderPicker()
	if a.pickerOpen {
		a.pickerQuery = ""
		go func() {
			var d shared.NoteList
			if fetchJSON("GET", apiURL("/api/notes"), nil, &d) {
				a.allNotes = d.Notes
				renderPickerList()
			} else {
				a.setStatus("便签列表加载失败", "error")
			}
		}()
	}
}

func (a *App) onPickerQuery(q string) {
	a.pickerQuery = q
	trimmed := strings.TrimSpace(q)
	if a.searchTimer.Truthy() {
		clearTimeout(a.searchTimer)
	}
	if trimmed == "" {
		go func() {
			var d shared.NoteList
			if fetchJSON("GET", apiURL("/api/notes"), nil, &d) {
				a.allNotes = d.Notes
				renderPickerList()
			}
		}()
		return
	}
	a.searchTimer = setTimeout(300, func() {
		go func() {
			var d shared.NoteList
			if fetchJSON("GET", apiURL("/api/search?q="+encodeURIComponent(trimmed)), nil, &d) {
				a.allNotes = d.Notes
				renderPickerList()
			} else {
				a.setStatus("搜索失败", "error")
			}
		}()
	})
}

func renderPicker() {
	if app.pickerPanel.Truthy() {
		if app.pickerOpen {
			app.pickerPanel.Set("style", "")
		} else {
			app.pickerPanel.Set("style", "display:none")
		}
	}
}

func renderPickerList() {
	list := app.pickerList
	if !list.Truthy() {
		return
	}
	list.Set("innerHTML", "")
	if len(app.allNotes) == 0 {
		list.Call("appendChild", el("li", map[string]any{"class": "picker-empty"}, "没有匹配的便签"))
		return
	}
	for _, n := range app.allNotes {
		note := n
		li := el("li", nil)
		rowMain := el("span", map[string]any{"class": "row-main"})
		rowMain.Call("appendChild", el("span", map[string]any{"class": "row-title"}, note.Title))
		rowMain.Call("appendChild", el("span", map[string]any{"class": "row-id"}, BASE_PATH+"/"+note.ID))
		li.Call("appendChild", rowMain)
		if note.Title == "" && note.Excerpt != "" {
			li.Call("appendChild", el("span", map[string]any{"class": "row-excerpt"}, note.Excerpt))
		}
		rowMeta := el("span", map[string]any{"class": "row-meta"})
		for _, t := range note.Tags {
			rowMeta.Call("appendChild", el("span", map[string]any{"class": "chip"}, t))
		}
		if note.Author != "" {
			rowMeta.Call("appendChild", el("span", map[string]any{"class": "row-author"}, "✍️ "+note.Author))
		}
		date := note.UpdatedAt
		if len(date) >= 10 {
			date = date[:10]
		}
		rowMeta.Call("appendChild", el("span", map[string]any{"class": "row-date"}, date))
		li.Call("appendChild", rowMeta)
		on(li, "click", func(e jsValue) {
			js.Global().Get("location").Set("href", BASE_PATH+"/"+note.ID)
		})
		list.Call("appendChild", li)
	}
}

// ---- admin / request panels ----

func renderAdminTools() {
	a := app
	if !a.lockBtn.Truthy() {
		return
	}
	if a.isAdmin {
		a.lockBtn.Set("style", "")
		if a.editLocked {
			a.lockBtn.Set("textContent", "🔓 解锁编辑")
		} else {
			a.lockBtn.Set("textContent", "🔒 锁定编辑")
		}
	} else {
		a.lockBtn.Set("style", "display:none")
	}
	if a.isAdmin && a.editLocked {
		a.reqSpan.Set("style", "")
		a.reqBtn.Set("textContent", "📝 申请 ("+strconv.Itoa(a.pendingCount())+")")
	} else {
		a.reqSpan.Set("style", "display:none")
	}
}

func renderReqPanel() {
	a := app
	if !a.reqListPanel.Truthy() {
		return
	}
	if !a.reqPanelOpen {
		a.reqListPanel.Set("style", "display:none")
		return
	}
	a.reqListPanel.Set("style", "")
	a.reqList.Set("innerHTML", "")
	if len(a.editRequests) == 0 {
		a.reqList.Call("appendChild", el("li", map[string]any{"class": "req-empty"}, "暂无申请"))
		return
	}
	for _, r := range a.editRequests {
		req := r
		li := el("li", nil)
		row := el("span", map[string]any{"class": "req-row"})
		row.Call("appendChild", el("strong", nil, req.Username))
		cls := "req-status"
		label := "待审批"
		if req.Status == "approved" {
			cls = "req-status approved"
			label = "已批准"
		}
		row.Call("appendChild", el("span", map[string]any{"class": cls}, label))
		if req.Message != "" {
			row.Call("appendChild", el("span", map[string]any{"class": "req-msg"}, req.Message))
		}
		when := req.CreatedAt
		if len(when) >= 16 {
			when = when[:16]
		}
		row.Call("appendChild", el("span", map[string]any{"class": "req-date"}, when))
		actions := el("span", map[string]any{"class": "req-actions"})
		if req.Status == "approved" {
			b := el("button", map[string]any{"type": "button", "class": "tool"}, "撤销")
			on(b, "click", func(e jsValue) { a.handleEditRequest(req.Username, "revoke") })
			actions.Call("appendChild", b)
		} else {
			ba := el("button", map[string]any{"type": "button", "class": "tool"}, "同意")
			on(ba, "click", func(e jsValue) { a.handleEditRequest(req.Username, "approve") })
			br := el("button", map[string]any{"type": "button", "class": "tool"}, "拒绝")
			on(br, "click", func(e jsValue) { a.handleEditRequest(req.Username, "reject") })
			actions.Call("appendChild", ba)
			actions.Call("appendChild", br)
		}
		row.Call("appendChild", actions)
		li.Call("appendChild", row)
		a.reqList.Call("appendChild", li)
	}
}

// updateEditability reflects can_edit / mark_mode on the title/tags inputs.
func updateEditability() {
	a := app
	disabled := !a.canEdit || a.markMode
	if a.titleInput.Truthy() {
		a.titleInput.Set("disabled", disabled)
	}
	if a.tagsInput.Truthy() {
		a.tagsInput.Set("disabled", disabled)
	}
	if a.uploadBtn.Truthy() {
		if a.canEdit {
			a.uploadBtn.Set("style", "")
			a.appendBtn.Set("style", "")
		} else {
			a.uploadBtn.Set("style", "display:none")
			a.appendBtn.Set("style", "display:none")
		}
	}
}

// ---- lock banner / apply panel ----

func renderLockBanner() {
	a := app
	// banner is rendered imperatively so it can host the apply panel
	old := qs(".lock-banner")
	if old.Truthy() {
		old.Call("remove")
	}
	oldCard := qs(".lock-apply-panel")
	if oldCard.Truthy() {
		oldCard.Call("remove")
	}
	if !(a.editLocked && !a.canEdit && !a.lockDismissed) {
		return
	}
	banner := el("div", map[string]any{"class": "lock-banner"})
	banner.Call("appendChild", el("span", map[string]any{"class": "lock-banner-text"}, "🔒 编辑已锁定 — 你可以阅读本文，申请通过后即可编辑"))
	if a.editRequest != "pending" {
		b := el("button", map[string]any{"type": "button", "class": "tool"}, "申请编辑权限")
		on(b, "click", func(e jsValue) {
			a.lockApplyOpen = true
			renderLockBanner()
		})
		banner.Call("appendChild", b)
	} else {
		banner.Call("appendChild", el("span", map[string]any{"class": "lock-pending-inline"}, "申请已提交，等待审批…"))
	}
	close := el("button", map[string]any{"type": "button", "class": "tool lock-close", "title": "关闭提示(不影响阅读)"}, "×")
	on(close, "click", func(e jsValue) {
		a.lockDismissed = true
		renderLockBanner()
	})
	banner.Call("appendChild", close)
	app.editorWrapParent().Call("appendChild", banner)

	if a.lockApplyOpen && a.editLocked && !a.canEdit {
		panel := el("div", map[string]any{"class": "lock-apply-panel"})
		card := el("div", map[string]any{"class": "lock-card"})
		head := el("div", map[string]any{"class": "lock-card-head"})
		head.Call("appendChild", el("strong", nil, "申请编辑权限"))
		x := el("button", map[string]any{"type": "button", "class": "tool lock-close"}, "×")
		on(x, "click", func(e jsValue) {
			a.lockApplyOpen = false
			renderLockBanner()
		})
		head.Call("appendChild", x)
		card.Call("appendChild", head)
		if a.editRequest == "pending" {
			card.Call("appendChild", el("p", map[string]any{"class": "lock-pending"}, "已提交申请，等待管理员审批…"))
		} else {
			form := el("div", map[string]any{"class": "lock-form"})
			input := el("input", map[string]any{"class": "lock-input", "placeholder": "申请理由（可选）", "maxlength": "200"})
			input.Set("value", a.reqMessage)
			on(input, "input", func(e jsValue) { a.reqMessage = input.Get("value").String() })
			on(input, "keydown", func(e jsValue) {
				if e.Get("key").String() == "Enter" {
					a.submitEditRequest()
					a.lockApplyOpen = false
					renderLockBanner()
				}
			})
			btn := el("button", map[string]any{"type": "button", "class": "lock-btn"}, "提交申请")
			on(btn, "click", func(e jsValue) {
				a.submitEditRequest()
				a.lockApplyOpen = false
				renderLockBanner()
			})
			form.Call("appendChild", input)
			form.Call("appendChild", btn)
			card.Call("appendChild", form)
		}
		panel.Call("appendChild", card)
		app.editorWrapParent().Call("appendChild", panel)
	}
}

func (a *App) editorWrapParent() jsValue {
	w := qs(".editor-wrap")
	if w.Truthy() {
		return w
	}
	return body()
}

// ---- save ----

func (a *App) markDirty() {
	a.dirty = true
	a.setStatus("有未保存的更改", "saving")
}

func (a *App) notePayload() string {
	content := storageMD(a.editor.getValue())
	return `{"content":` + jsonStr(content) +
		`,"font_size":` + strconv.FormatFloat(float64(a.fontSize), 'f', 1, 64) +
		`,"color":` + jsonStr(a.color) +
		`,"title":` + jsonStr(a.title) +
		`,"tags":` + jsonStrs(a.parsedTags()) + `}`
}

// watchBaseline is the Leptos effect comparing current settings against the
// load-time baseline: a real change marks the note dirty.
func (a *App) watchBaseline() {
	if !a.canEdit || a.markMode {
		return
	}
	if !a.loaded || a.applyingRemote {
		return
	}
	if a.fontSize != a.baselineFont || a.color != a.baselineColor ||
		a.title != a.baselineTitle || a.tagsText != a.baselineTags {
		a.baselineFont = a.fontSize
		a.baselineColor = a.color
		a.baselineTitle = a.title
		a.baselineTags = a.tagsText
		a.markDirty()
	}
}

func (a *App) save() {
	if a.markMode {
		a.saveMarkOnly()
		return
	}
	go func() {
		resp := fetchDo("PUT", apiURL("/api/note/"+a.id), js.ValueOf(a.notePayload()), map[string]string{"Content-Type": "application/json"})
		status := resp.Get("status").Int()
		switch {
		case status == 401:
			js.Global().Get("location").Call("reload")
		case status == 403:
			a.refreshEditStatus()
			a.dirty = false
			a.setStatus("编辑已被锁定，本次修改未保存", "error")
		case resp.Get("ok").Bool():
			var d shared.Note
			j := await(resp.Call("json"))
			if json.Unmarshal([]byte(js.Global().Get("JSON").Call("stringify", j).String()), &d) == nil {
				a.pollUpdatedAt = d.UpdatedAt
				a.dirty = false
				a.suppressInputUntil = nowMs() + 300
				if d.Author != "" {
					a.author = d.Author
					renderAuthor()
				}
				a.remoteChanged = false
				renderBanners()
				a.setStatus("已保存 "+currentTime(), "saved")
			}
		default:
			a.setStatus("保存失败，将重试", "error")
			setTimeout(2000, func() { a.save() })
		}
	}()
}

func (a *App) saveNow() {
	if !a.canEdit {
		a.setStatus("编辑已锁定，无法保存", "error")
		return
	}
	if !a.dirty {
		a.setStatus("没有更改需要保存", "")
		return
	}
	a.save()
}

// ---- remote sync ----

func (a *App) applyRemote(d *shared.Note) {
	a.applyingRemote = true
	a.editor.setValue(displayMD(d.Content))
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
	a.applyingRemote = false
	a.pollUpdatedAt = d.UpdatedAt
	a.dirty = false
	a.loaded = true
	a.suppressInputUntil = nowMs() + 400
	a.lastApplied = d.Content
	a.baselineFont = shared.ResolveFontSize(float64(d.FontSize))
	a.baselineColor = d.Color
	a.baselineTitle = d.Title
	a.baselineTags = strings.Join(d.Tags, ", ")
	a.remoteChanged = false
	renderBanners()
}

func (a *App) pollNote() {
	if a.applyingRemote || a.showLogin || a.markMode {
		return
	}
	go func() {
		resp := fetchDo("GET", apiURL("/api/note/"+a.id), nil, nil)
		status := resp.Get("status").Int()
		switch {
		case status == 401:
			// stop polling on session expiry
		case status >= 200 && status < 300:
			var d shared.Note
			j := await(resp.Call("json"))
			if json.Unmarshal([]byte(js.Global().Get("JSON").Call("stringify", j).String()), &d) != nil {
				return
			}
			if a.pollUpdatedAt == "" || d.UpdatedAt == a.pollUpdatedAt {
				return
			}
			if a.dirty {
				a.remoteChanged = true
				renderBanners()
			} else {
				who := d.Author
				if who == "" {
					who = "他人"
				}
				a.applyRemote(&d)
				a.setStatus("已同步 "+who+" 的更新", "saved")
			}
		}
	}()
}

func (a *App) loadLatest() {
	go func() {
		var d shared.Note
		if fetchJSON("GET", apiURL("/api/note/"+a.id), nil, &d) {
			a.applyRemote(&d)
			a.setStatus("已载入最新内容", "saved")
		} else {
			a.setStatus("加载失败", "error")
		}
	}()
}

// ---- presence ----

func (a *App) beatPresence() {
	go func() {
		var d shared.PresenceResponse
		if fetchJSON("POST", apiURL("/api/presence"), shared.PresenceRequest{NoteID: a.id}, &d) {
			others := []string{}
			for _, u := range d.Users {
				if u != a.user {
					others = append(others, u)
				}
			}
			a.presenceOthers = others
			renderPresence()
		}
	}()
}

// ---- font / zoom ----

func (a *App) bumpFont(delta int) {
	a.setFontSize(a.fontSize + delta)
}

func (a *App) setFontSize(n int) {
	if n < shared.MinFontSize {
		n = shared.MinFontSize
	}
	if n > shared.MaxFontSize {
		n = shared.MaxFontSize
	}
	a.fontSize = n
	a.applyEditorStyle()
	renderFontReset()
	// text re-layouts; re-render overlay marks after a beat
	setTimeout(250, func() { a.refreshOverlayMarks() })
	a.watchBaseline()
}

// jsonStr marshals a string as a JSON string literal.
func jsonStr(s string) string {
	b, _ := json.Marshal(s)
	return string(b)
}

func jsonStrs(s []string) string {
	b, _ := json.Marshal(s)
	return string(b)
}
