//go:build js && wasm

package main

import (
	"syscall/js"

	"urlnote/goback/shared"
)

// ---- notes_mark editing mode ----
// The editor shows the notes_mark copy and saves go there; the clean
// original stays in the database and is restored on exit.

func (a *App) refreshMarkStatus() {
	go func() {
		resp := fetchDo("GET", apiURL("/api/mark/"+a.id), nil, nil)
		a.markExists = resp.Get("ok").Bool()
		renderPosLegend()
	}()
}

func (a *App) generateMark() {
	go func() {
		status, ok := fetchStatus("POST", apiURL("/api/mark/"+a.id), nil)
		switch {
		case ok:
			a.markExists = true
			renderPosLegend()
			a.setStatus("标记文档已生成(notes_mark)", "saved")
		case status == 403:
			a.setStatus("编辑已锁定", "error")
		default:
			a.setStatus("生成标记失败", "error")
		}
	}()
}

func (a *App) removeMark() {
	if !confirm("取消标记将删除标记文档(原便签不受影响),确定？") {
		return
	}
	go func() {
		if _, ok := fetchStatus("DELETE", apiURL("/api/mark/"+a.id), nil); ok {
			a.markExists = false
			renderPosLegend()
			a.setStatus("已取消标记", "saved")
		} else {
			a.setStatus("取消标记失败", "error")
		}
	}()
}

// exportMark downloads the marked copy as a .md file.
func (a *App) exportMark() {
	go func() {
		var info shared.MarkInfo
		if !fetchJSON("GET", apiURL("/api/mark/"+a.id), nil, &info) {
			a.setStatus("没有标记文档", "error")
			return
		}
		downloadText(a.id+"-marked.md", info.Content)
		a.setStatus("已导出标记文档", "saved")
	}()
}

func (a *App) enterMarkMode() {
	if a.dirty {
		// persist pending original edits first, then enter
		a.save()
		setTimeout(700, func() { a.doEnterMarkMode() })
		return
	}
	a.doEnterMarkMode()
}

func (a *App) doEnterMarkMode() {
	go func() {
		var info shared.MarkInfo
		if !fetchJSON("GET", apiURL("/api/mark/"+a.id), nil, &info) {
			a.setStatus("没有标记文档,请先生成标记", "error")
			return
		}
		a.editor.setValue(displayMD(info.Content))
		a.dirty = false
		a.pollUpdatedAt = ""
		a.lastApplied = info.Content
		a.markMode = true
		renderBanners()
		updateEditability()
		a.setStatus("正在编辑标记文档(保存只写入 notes_mark)", "saved")
	}()
}

// exitMarkMode saves any pending mark edits, then reloads the clean original.
func (a *App) exitMarkMode() {
	if a.dirty {
		go func() {
			a.saveMarkOnly()
			// give the save a moment, then restore the clean note
			setTimeout(600, func() { a.loadCleanNote() })
		}()
	} else {
		a.loadCleanNote()
	}
}

func (a *App) loadCleanNote() {
	go func() {
		var d shared.Note
		if fetchJSON("GET", apiURL("/api/note/"+a.id), nil, &d) {
			a.applyRemote(&d)
			a.markMode = false
			renderBanners()
			updateEditability()
			a.setStatus("已返回原文", "saved")
		}
	}()
}

// saveMarkOnly persists editor content into notes_mark only.
func (a *App) saveMarkOnly() {
	content := storageMD(a.editor.getValue())
	go func() {
		resp := fetchDo("PUT", apiURL("/api/mark/"+a.id), js.ValueOf(`{"content":`+jsonStr(content)+`}`), map[string]string{"Content-Type": "application/json"})
		status := resp.Get("status").Int()
		switch {
		case status == 401:
			js.Global().Get("location").Call("reload")
		case resp.Get("ok").Bool():
			a.dirty = false
			a.suppressInputUntil = nowMs() + 300
			a.setStatus("已保存标记 "+currentTime(), "saved")
		case status == 403:
			a.setStatus("编辑已锁定", "error")
		default:
			a.setStatus("保存标记失败,将重试", "error")
		}
	}()
}
