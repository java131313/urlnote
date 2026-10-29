//go:build js && wasm

package main

import (
	"strconv"
	"strings"
	"syscall/js"

	"urlnote/goback/shared"
)

// ---- POS annotation overlay & in-editor find ----
// This file ports the Rust overlay geometry verbatim: collect_editor_text,
// prep_nodes, MarkSpace, rects_for_ranges, draw_marks and the shared scroll
// translator.

// textNode is (node, text, charStart).
type textNode struct {
	node  jsValue
	text  string
	start int
}

// collectTextNodes walks editable text nodes (skipping code blocks etc.)
// with their global char offsets.
func collectTextNodes(el jsValue, out *[]textNode, acc *int) {
	children := el.Get("childNodes")
	for i := 0; i < children.Get("length").Int(); i++ {
		node := children.Call("item", i)
		if !node.Truthy() {
			continue
		}
		switch node.Get("nodeType").Int() {
		case 3: // TEXT_NODE
			text := node.Get("textContent").String()
			if strings.TrimSpace(text) != "" {
				*out = append(*out, textNode{node: node, text: text, start: *acc})
			}
			*acc += len([]rune(text))
		case 1: // ELEMENT_NODE
			// Vditor wraps every wysiwyg block in <pre class="vditor-reset">
			// — only <code> (real code blocks), style and script containers
			// are skipped
			if node.Call("closest", "code, style, script").Truthy() {
				// still advance the offset so positions stay aligned
				*acc += len([]rune(node.Get("textContent").String()))
			} else {
				collectTextNodes(node, out, acc)
			}
		}
	}
}

// collectEditorText returns the editable text nodes plus concatenated text.
func collectEditorText() ([]textNode, string, bool) {
	contentEl := qs(".editor .vditor-content")
	if !contentEl.Truthy() {
		return nil, "", false
	}
	var nodes []textNode
	acc := 0
	collectTextNodes(contentEl, &nodes, &acc)
	var full strings.Builder
	for _, n := range nodes {
		full.WriteString(n.text)
	}
	return nodes, full.String(), true
}

// prepNode carries char length + UTF-16 prefix sums (DOM offsets are UTF-16
// units; prefix[k] = UTF-16 length of the first k chars).
type prepNode struct {
	node   jsValue
	start  int
	chars  int
	prefix []int
}

func prepNodes(nodes []textNode) []prepNode {
	out := make([]prepNode, 0, len(nodes))
	for _, n := range nodes {
		prefix := make([]int, 0, len([]rune(n.text))+1)
		prefix = append(prefix, 0)
		for _, ch := range n.text {
			prefix = append(prefix, prefix[len(prefix)-1]+utf16Len(ch))
		}
		out = append(out, prepNode{node: n.node, start: n.start, chars: len([]rune(n.text)), prefix: prefix})
	}
	return out
}

func utf16Len(r rune) int {
	if r > 0xFFFF {
		return 2
	}
	return 1
}

// markSpace is the coordinate mapping for mark boxes. The overlay sits in
// .editor-wrap, which starts above .vditor-content (toolbar strip) and is
// NOT affected by the editor's CSS zoom — so rect deltas must be re-based
// onto the overlay origin (ox/oy) and de-zoomed.
type markSpace struct {
	zoom       float64
	cx, cy     float64
	ox, oy     float64
	scrollLeft float64
	scrollTop  float64
}

func buildMarkSpace(contentEl, overlay jsValue) markSpace {
	cr := contentEl.Call("getBoundingClientRect")
	or := overlay.Call("getBoundingClientRect")
	sp := markSpace{
		zoom: float64(app.fontSize) / float64(shared.DefaultFontSize),
		cx:   cr.Get("left").Float(),
		cy:   cr.Get("top").Float(),
		ox:   cr.Get("left").Float() - or.Get("left").Float(),
		oy:   cr.Get("top").Float() - or.Get("top").Float(),
	}
	host := scrollHost(contentEl)
	sp.scrollLeft = host.Get("scrollLeft").Float()
	sp.scrollTop = host.Get("scrollTop").Float()
	return sp
}

// xy maps content viewport coordinates into overlay space, baking the
// current scroll (content units × zoom) so the layer transform only tracks
// scroll *changes*.
func (sp markSpace) xy(left, top float64) (float64, float64) {
	x := (left - sp.cx) + sp.ox + sp.scrollLeft*sp.zoom
	y := (top - sp.cy) + sp.oy + sp.scrollTop*sp.zoom
	return x, y
}

// scrollHost: Vditor keeps every block in one <pre class="vditor-reset">
// that carries its own overflow — THAT element scrolls, not .vditor-content.
func scrollHost(contentEl jsValue) jsValue {
	if pre := contentEl.Call("querySelector", "pre.vditor-reset"); pre.Truthy() {
		if pre.Get("scrollHeight").Int() > pre.Get("clientHeight").Int() ||
			pre.Get("scrollWidth").Int() > pre.Get("clientWidth").Int() {
			return pre
		}
	}
	return contentEl
}

func overlayScrollOffset(contentEl jsValue) (float64, float64) {
	zoom := float64(app.fontSize) / float64(shared.DefaultFontSize)
	host := scrollHost(contentEl)
	return host.Get("scrollLeft").Float() * zoom, host.Get("scrollTop").Float() * zoom
}

type markBox struct {
	x, y, w, h float64
	class      string
}

// rectsForRanges computes mark boxes for char ranges. Reads layout only —
// the caller batches all rects before writing any DOM.
func rectsForRanges(prep []prepNode, ranges [][3]any, space markSpace, max int) []markBox {
	var out []markBox
	nodeIdx := 0
outer:
	for _, r := range ranges {
		s := r[0].(int)
		e := r[1].(int)
		class := r[2].(string)
		for nodeIdx < len(prep) && s >= prep[nodeIdx].start+prep[nodeIdx].chars {
			nodeIdx++
		}
		if nodeIdx >= len(prep) {
			break
		}
		pn := prep[nodeIdx]
		if e > pn.start+pn.chars {
			continue // spans nodes — rare, skip
		}
		su := pn.prefix[min(s-pn.start, pn.chars)]
		eu := pn.prefix[min(e-pn.start, pn.chars)]
		range_ := doc().Call("createRange")
		range_.Call("setStart", pn.node, su)
		range_.Call("setEnd", pn.node, eu)
		rects := range_.Call("getClientRects")
		for i := 0; i < rects.Get("length").Int(); i++ {
			rect := rects.Call("item", i)
			if rect.Get("width").Float() <= 0 || rect.Get("height").Float() <= 0 {
				continue
			}
			x, y := space.xy(rect.Get("left").Float(), rect.Get("top").Float())
			out = append(out, markBox{x: x, y: y, w: rect.Get("width").Float(), h: rect.Get("height").Float(), class: class})
			if len(out) >= max {
				break outer
			}
		}
	}
	return out
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}

// drawMarks draws mark boxes into a dedicated layer inside .pos-overlay.
// Also installs the shared scroll translator once.
func drawMarks(layerClass string, marks []markBox) bool {
	overlay := qs(".pos-overlay")
	if !overlay.Truthy() {
		return false
	}
	contentEl := qs(".editor .vditor-content")
	if !contentEl.Truthy() {
		return false
	}
	zoom := float64(app.fontSize) / float64(shared.DefaultFontSize)
	overlay.Set("style", "--mark-zoom: "+ftoa(zoom, 3)+";")
	var layer jsValue
	if l := overlay.Call("querySelector", "."+layerClass); l.Truthy() {
		l.Set("innerHTML", "")
		layer = l
	} else {
		layer = el("div", map[string]any{"class": layerClass})
		overlay.Call("appendChild", layer)
	}
	var html strings.Builder
	for _, m := range marks {
		html.WriteString(`<div class="` + m.class + `" style="left:` + ftoa(m.x, 1) + "px;top:" + ftoa(m.y, 1) + "px;width:" + ftoa(m.w, 1) + "px;height:" + ftoa(m.h, 1) + `px"></div>`)
	}
	layer.Set("innerHTML", html.String())
	sx, sy := overlayScrollOffset(contentEl)
	layer.Set("style", "transform: translate("+ftoa(-sx, 1)+"px, "+ftoa(-sy, 1)+"px);")
	ensureScrollListener(contentEl, overlay)
	return len(marks) > 0
}

// ensureScrollListener installs one capture-phase scroll listener that
// translates every overlay layer so marks track the scrolling content.
func ensureScrollListener(contentEl, overlay jsValue) {
	if app.posScrollDone {
		return
	}
	app.posScrollDone = true
	f := js.FuncOf(func(_ js.Value, _ []js.Value) any {
		zoom := float64(app.fontSize) / float64(shared.DefaultFontSize)
		host := scrollHost(contentEl)
		sx := host.Get("scrollLeft").Float() * zoom
		sy := host.Get("scrollTop").Float() * zoom
		children := overlay.Get("children")
		for i := 0; i < children.Get("length").Int(); i++ {
			children.Call("item", i).Set("style", "transform: translate("+ftoa(-sx, 1)+"px, "+ftoa(-sy, 1)+"px);")
		}
		return nil
	})
	keep(f)
	contentEl.Call("addEventListener", "scroll", f, true)
}

func ftoa(f float64, prec int) string {
	return strconv.FormatFloat(f, 'f', prec, 64)
}

// ---- POS refresh ----

func (a *App) togglePos() {
	a.posOn = !a.posOn
	if a.posOn {
		if a.posBtn.Truthy() {
			a.posBtn.Set("className", "tool pos-on")
		}
		a.schedulePosRefresh(true)
		a.refreshMarkStatus()
	} else {
		if a.posBtn.Truthy() {
			a.posBtn.Set("className", "tool")
		}
		if a.posDebounce.Truthy() {
			clearTimeout(a.posDebounce)
		}
		if ov := qs(".pos-overlay"); ov.Truthy() {
			ov.Set("innerHTML", "")
			ov.Call("appendChild", el("div", map[string]any{"class": "pos-layer"}))
		}
	}
	renderPosLegend()
}

func (a *App) schedulePosRefresh(immediate bool) {
	if !a.posOn {
		return
	}
	if a.posDebounce.Truthy() {
		clearTimeout(a.posDebounce)
	}
	delay := 800
	if immediate {
		delay = 0
	}
	a.posDebounce = setTimeout(delay, func() { a.doPosRefresh() })
}

func (a *App) doPosRefresh() {
	if !a.posOn {
		return
	}
	nodes, fullText, ok := collectEditorText()
	if !ok {
		return
	}
	a.posSeq++
	seq := a.posSeq
	id := a.id
	currentUpdated := a.pollUpdatedAt
	go func() {
		var tokens []shared.PosToken
		taggedChars := 0
		// a FRESH notes_mark copy is the preferred annotation source — no
		// tokenizer round-trip; stale or hand-edited marks fall through to
		// the live /api/pos pass
		var info shared.MarkInfo
		if fetchJSON("GET", apiURL("/api/mark/"+id), nil, &info) &&
			info.SourceUpdatedAt != "" && info.SourceUpdatedAt == currentUpdated {
			a.markExists = true
			renderPosLegend()
			toks, clean := parseMarkedTokens(info.Content)
			taggedChars = len([]rune(info.Content))
			// token offsets are relative to the clean note text; the editor
			// walk includes UI chrome before the content — shift to match
			if len(clean) > 40 {
				clean = clean[:40]
			}
			if byte := strings.Index(fullText, clean); byte >= 0 {
				shift := len([]rune(fullText[:byte]))
				for i := range toks {
					toks[i].S += shift
					toks[i].E += shift
				}
			}
			tokens = toks
		} else {
			var d shared.PosResponse
			if !fetchJSON("POST", apiURL("/api/pos"), shared.AnalyzeRequest{Content: fullText}, &d) {
				return
			}
			taggedChars = d.TaggedChars
			tokens = d.Tokens
		}
		if !a.posOn || a.posSeq != seq {
			return // toggled off or a newer refresh superseded us
		}
		a.posTaggedChars = taggedChars
		a.posTokens = tokens
		drawn := renderPosMarks(nodes, tokens)
		if !drawn && len(tokens) > 0 {
			// transient editor state (mid-render) can yield zero rects —
			// retry once shortly
			setTimeout(600, func() {
				if a.posOn {
					a.schedulePosRefresh(true)
				}
			})
		}
	}()
}

func renderPosMarks(nodes []textNode, tokens []shared.PosToken) bool {
	contentEl := qs(".editor .vditor-content")
	overlay := qs(".pos-overlay")
	if !contentEl.Truthy() || !overlay.Truthy() {
		return false
	}
	space := buildMarkSpace(contentEl, overlay)
	prep := prepNodes(nodes)
	ranges := make([][3]any, 0, len(tokens))
	for _, t := range tokens {
		ranges = append(ranges, [3]any{t.S, t.E, "pos-mark cat-" + t.Cat})
	}
	marks := rectsForRanges(prep, ranges, space, 5000)
	drawn := drawMarks("pos-layer", marks)
	return drawn || len(tokens) == 0
}

// parseMarkedTokens parses `<span class="posw cat-X">word</span>` tags out
// of a marked document, producing tokens with char offsets relative to the
// CLEAN text (tags stripped).
func parseMarkedTokens(marked string) ([]shared.PosToken, string) {
	const open = `<span class="posw cat-`
	var tokens []shared.PosToken
	var clean strings.Builder
	rest := marked
	cleanBase := 0
	for {
		i := strings.Index(rest, open)
		if i < 0 {
			break
		}
		rel := rest[i+len(open):]
		q := strings.Index(rel, `"`)
		if q < 0 {
			break
		}
		cat := rel[:q]
		gt := strings.Index(rest[i:], ">")
		if gt < 0 {
			break
		}
		bodyStart := i + gt + 1
		endRel := strings.Index(rest[bodyStart:], "</span>")
		if endRel < 0 {
			break
		}
		bodyEnd := bodyStart + endRel
		prefix := stripPosw(rest[:i])
		cleanBase += len([]rune(prefix))
		clean.WriteString(prefix)
		word := rest[bodyStart:bodyEnd]
		tokens = append(tokens, shared.PosToken{W: word, Cat: cat, S: cleanBase, E: cleanBase + len([]rune(word))})
		cleanBase += len([]rune(word))
		clean.WriteString(word)
		rest = rest[bodyEnd+len("</span>"):]
	}
	clean.WriteString(stripPosw(rest))
	return tokens, clean.String()
}

// stripPosw removes all posw span tags from a marked fragment (content kept).
func stripPosw(s string) string {
	cats := []string{"n", "v", "a", "d", "u", "r", "m", "q", "c", "p", "aux", "x", "f"}
	for _, c := range cats {
		s = strings.ReplaceAll(s, `<span class="posw cat-`+c+`">`, "")
	}
	s = strings.ReplaceAll(s, "</span>", "")
	return s
}

// ---- find ----

func (a *App) toggleFind() {
	a.findOpen = !a.findOpen
	renderFindBar()
	if a.findOpen {
		setTimeout(60, func() {
			if inp := qs(".find-input"); inp.Truthy() {
				inp.Call("focus")
			}
		})
	} else {
		a.findQuery = ""
		a.findCount = 0
		a.findAll = 0
		if a.findDebounce.Truthy() {
			clearTimeout(a.findDebounce)
		}
		if l := qs(".find-layer"); l.Truthy() {
			l.Set("innerHTML", "")
		}
		renderFindBar()
	}
}

func renderFindBar() {
	a := app
	if !a.findInput.Truthy() {
		return
	}
	if a.findOpen {
		a.findInput.Set("style", "")
		a.findBtn.Set("className", "tool pos-on")
	} else {
		a.findInput.Set("style", "display:none")
		a.findBtn.Set("className", "tool")
	}
	if a.findOpen && strings.TrimSpace(a.findQuery) != "" {
		a.findMeta.Set("style", "")
		a.findMeta.Set("textContent", itoa(a.findCount)+" 处")
		if a.findAll != 0 {
			a.findMetaAll.Set("style", "")
			a.findMetaAll.Set("textContent", "全站 "+itoa(a.findAll)+" 条")
		} else {
			a.findMetaAll.Set("style", "display:none")
		}
	} else {
		a.findMeta.Set("style", "display:none")
		a.findMetaAll.Set("style", "display:none")
	}
}

func (a *App) onFindQuery(q string) {
	a.findQuery = q
	if a.findDebounce.Truthy() {
		clearTimeout(a.findDebounce)
	}
	a.findDebounce = setTimeout(300, func() { a.runFind() })
}

func (a *App) runFind() {
	q := strings.TrimSpace(a.findQuery)
	if q == "" || !a.findOpen {
		if l := qs(".find-layer"); l.Truthy() {
			l.Set("innerHTML", "")
		}
		a.findCount = 0
		renderFindBar()
		return
	}
	_, full, ok := collectEditorText()
	if !ok {
		return
	}
	occ := findOccurrences(full, q)
	a.findCount = len(occ)
	a.findRanges = occ
	renderFindBar()
	a.drawFindMarks(occ)
	// cross-note info from the search API
	go func() {
		var d shared.NoteList
		if fetchJSON("GET", apiURL("/api/search?q="+encodeURIComponent(q)), nil, &d) && a.findOpen {
			a.findAll = len(d.Notes)
			renderFindBar()
		}
	}()
}

// findOccurrences returns case-insensitive (ASCII) occurrence positions.
func findOccurrences(text, query string) [][2]int {
	needle := []rune(strings.ToLower(query))
	if len(needle) == 0 || len(needle) > 64 {
		return nil
	}
	hay := []rune(text)
	if len(needle) > len(hay) {
		return nil
	}
	lower := []rune(strings.ToLower(text))
	var out [][2]int
	for i := 0; i+len(needle) <= len(lower); i++ {
		match := true
		for k := range needle {
			if lower[i+k] != needle[k] {
				match = false
				break
			}
		}
		if match {
			out = append(out, [2]int{i, i + len(needle)})
			if len(out) >= 500 {
				break
			}
		}
	}
	return out
}

// drawFindMarks highlights occurrences and scrolls the first into view.
func (a *App) drawFindMarks(occ [][2]int) bool {
	if len(occ) == 0 {
		if l := qs(".find-layer"); l.Truthy() {
			l.Set("innerHTML", "")
		}
		return false
	}
	contentEl := qs(".editor .vditor-content")
	overlay := qs(".pos-overlay")
	if !contentEl.Truthy() || !overlay.Truthy() {
		return false
	}
	space := buildMarkSpace(contentEl, overlay)
	nodes, _, ok := collectEditorText()
	if !ok {
		return false
	}
	prep := prepNodes(nodes)
	ranges := make([][3]any, 0, len(occ))
	for _, o := range occ {
		ranges = append(ranges, [3]any{o[0], o[1], "find-mark"})
	}
	marks := rectsForRanges(prep, ranges, space, 500)
	drawn := drawMarks("find-layer", marks)
	if len(marks) > 0 {
		// scroll the first match into view (content units via the zoom)
		target := int(maxf(0, (marks[0].y-space.oy-120)/space.zoom))
		scrollHost(contentEl).Set("scrollTop", target)
	}
	return drawn
}

// refreshOverlayMarks re-renders both mark layers from cached data (called
// when the editor zoom changes).
func (a *App) refreshOverlayMarks() {
	if a.posOn && len(a.posTokens) > 0 {
		if nodes, _, ok := collectEditorText(); ok {
			renderPosMarks(nodes, a.posTokens)
		}
	}
	if a.findOpen && len(a.findRanges) > 0 {
		a.drawFindMarks(a.findRanges)
	}
}

func maxf(a, b float64) float64 {
	if a > b {
		return a
	}
	return b
}

// ---- pos legend ----

func renderPosLegend() {
	a := app
	if !a.posLegend.Truthy() {
		return
	}
	if !a.posOn {
		a.posLegend.Set("style", "display:none")
		return
	}
	a.posLegend.Set("style", "")
	a.posLegend.Set("innerHTML", "")
	if !a.markExists {
		btn := el("button", map[string]any{"type": "button", "class": "tool pos-doc-btn", "title": "生成含词性标注的 Markdown 副本,存入 notes_mark(原便签不变)"}, "生成标记")
		on(btn, "click", func(e jsValue) { a.generateMark() })
		a.posLegend.Call("appendChild", btn)
	} else {
		if !a.markMode {
			btn := el("button", map[string]any{"type": "button", "class": "tool pos-doc-btn", "title": "在编辑器中编辑标记文档(保存只写入 notes_mark,不影响原文)"}, "编辑标记")
			on(btn, "click", func(e jsValue) { a.enterMarkMode() })
			a.posLegend.Call("appendChild", btn)
		}
		b2 := el("button", map[string]any{"type": "button", "class": "tool pos-doc-btn", "title": "下载 notes_mark 中的标记文档(.md)"}, "导出标记")
		on(b2, "click", func(e jsValue) { a.exportMark() })
		b3 := el("button", map[string]any{"type": "button", "class": "tool pos-doc-btn", "title": "删除标记文档,原便签保持不变"}, "取消标记")
		on(b3, "click", func(e jsValue) { a.removeMark() })
		a.posLegend.Call("appendChild", b2)
		a.posLegend.Call("appendChild", b3)
	}
	legend := [][2]string{
		{"cat-n", "名词"}, {"cat-v", "动词"}, {"cat-a", "形容词"}, {"cat-d", "副词"},
		{"cat-u", "助词"}, {"cat-r", "代词"}, {"cat-c", "连词"}, {"cat-aux", "助动词"}, {"cat-x", "其他"},
	}
	for _, l := range legend {
		span := el("span", nil)
		span.Call("appendChild", el("i", map[string]any{"class": "dot " + l[0]}))
		span.Call("appendChild", doc().Call("createTextNode", l[1]))
		a.posLegend.Call("appendChild", span)
	}
	// truncation note
	total := len([]rune(a.editor.getValue()))
	if a.posTaggedChars < total {
		a.posLegend.Call("appendChild", el("em", map[string]any{"class": "pos-truncated"}, "仅标注前 "+itoa(a.posTaggedChars)+" 字"))
	}
}
