//go:build js && wasm

package main

// Object-storage file browser + markdown editor page (route /oss).
// Lists the bucket's directory structure, opens markdown files in a
// standalone Vditor instance, and saves back to the bucket — bypassing the
// note API entirely. S3 requests are SigV4-signed in the browser against the
// Sealos object-storage endpoint (MinIO-compatible).
//
// SECURITY: OSSAK/OSSSK are compile-time constants baked into the public
// WASM bundle. Anyone can extract them and access the bucket — fine for
// personal use (same tradeoff as the Rust build it ports).

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/xml"
	"sort"
	"strconv"
	"strings"
	"syscall/js"
)

// OSS config: set via -ldflags "-X main.OSSAK=..." at build time.
var (
	OSSEndpoint = "objectstorageapi.bja.sealos.run"
	OSSAK       string
	OSSSK       string
	OSSBucket   string
	OSSRegion   = "us-east-1"
)

func ossEnabled() bool { return OSSAK != "" && OSSBucket != "" }

// ---- SigV4 ----

func sha256Hex(b []byte) string {
	h := sha256.Sum256(b)
	return hex.EncodeToString(h[:])
}

func hmacSHA256(key, msg []byte) []byte {
	m := hmac.New(sha256.New, key)
	m.Write(msg)
	return m.Sum(nil)
}

// awsURIEncode encodes per RFC 3986 (S3 variant). encodeSlash controls
// whether '/' becomes %2F (query components) or stays (object keys).
func awsURIEncode(s string, encodeSlash bool) string {
	var out strings.Builder
	for i := 0; i < len(s); i++ {
		b := s[i]
		switch {
		case (b >= 'A' && b <= 'Z') || (b >= 'a' && b <= 'z') || (b >= '0' && b <= '9') ||
			b == '-' || b == '_' || b == '.' || b == '~':
			out.WriteByte(b)
		case b == '/' && !encodeSlash:
			out.WriteByte('/')
		default:
			out.WriteString("%" + strings.ToUpper(strconv.FormatUint(uint64(b), 16)))
			if b < 16 {
				// FormatUint has no zero padding — pad manually
			}
		}
	}
	return padHexEscapes(out.String())
}

// padHexEscapes zero-pads %X escapes to %XX (cheap correctness over cleverness).
func padHexEscapes(s string) string {
	if !strings.Contains(s, "%") {
		return s
	}
	var out strings.Builder
	for i := 0; i < len(s); i++ {
		if s[i] == '%' && i+2 < len(s) && s[i+2] != '%' && isHexDigit(s[i+1]) && isHexDigit(s[i+2]) {
			out.WriteString(s[i : i+3])
			i += 2
			continue
		}
		if s[i] == '%' && i+1 < len(s) && isHexDigit(s[i+1]) {
			out.WriteString("%0" + string(s[i+1]))
			i++
			continue
		}
		out.WriteByte(s[i])
	}
	return out.String()
}

func isHexDigit(b byte) bool {
	return (b >= '0' && b <= '9') || (b >= 'a' && b <= 'f') || (b >= 'A' && b <= 'F')
}

// fmtAmzTimes returns (amz_date, date_stamp) for epoch milliseconds in UTC.
func fmtAmzTimes(ms float64) (string, string) {
	secs := int64(ms / 1000)
	days := secs / 86400
	rem := secs % 86400
	hh, mi, ss := rem/3600, (rem%3600)/60, rem%60
	y, m, d := civilFromDays(days)
	return itoa4(y) + itoa2(m) + itoa2(d) + "T" + itoa2(int(hh)) + itoa2(int(mi)) + itoa2(int(ss)) + "Z",
		itoa4(y) + itoa2(m) + itoa2(d)
}

// civilFromDays is Howard Hinnant's algorithm (days since epoch → y/m/d).
func civilFromDays(z int64) (int, int, int) {
	z += 719468
	era := z / 146097
	if z < 0 && z%146097 != 0 {
		era--
	}
	doe := z - era*146097
	yoe := (doe - doe/1460 + doe/36524 - doe/146096) / 365
	y := yoe + era*400
	doy := doe - (365*yoe + yoe/4 - yoe/100)
	mp := (5*doy + 2) / 153
	d := doy - (153*mp+2)/5 + 1
	m := mp + 3
	if mp >= 10 {
		m = mp - 9
	}
	if m <= 2 {
		y++
	}
	return int(y), int(m), int(d)
}

func itoa4(n int) string {
	s := strconv.Itoa(n)
	for len(s) < 4 {
		s = "0" + s
	}
	return s
}

func itoa2(n int) string {
	s := strconv.Itoa(n)
	for len(s) < 2 {
		s = "0" + s
	}
	return s
}

type sigHeaders struct {
	authorization     string
	xAmzDate          string
	xAmzContentSHA256 string
}

// canonicalQuery sorts and encodes the query — used verbatim both in the
// signature and in the request URL.
func canonicalQuery(query [][2]string) string {
	enc := make([][2]string, len(query))
	for i, kv := range query {
		enc[i] = [2]string{awsURIEncode(kv[0], true), awsURIEncode(kv[1], true)}
	}
	sort.Slice(enc, func(i, j int) bool {
		if enc[i][0] != enc[j][0] {
			return enc[i][0] < enc[j][0]
		}
		return enc[i][1] < enc[j][1]
	})
	parts := make([]string, len(enc))
	for i, kv := range enc {
		parts[i] = kv[0] + "=" + kv[1]
	}
	return strings.Join(parts, "&")
}

// signRequestWith signs a request. path must already be URI-encoded (slashes
// kept); query keys/values are encoded here. Only the three SigV4-required
// headers are signed.
func signRequestWith(ak, sk, region, method, path string, query [][2]string, payload []byte, nowMs float64) sigHeaders {
	amz, dateStamp := fmtAmzTimes(nowMs)
	payloadHash := sha256Hex(payload)
	queryStr := canonicalQuery(query)
	canonical := method + "\n" + path + "\n" + queryStr + "\n" +
		"host:" + OSSEndpoint + "\n" +
		"x-amz-content-sha256:" + payloadHash + "\n" +
		"x-amz-date:" + amz + "\n\n" +
		"host;x-amz-content-sha256;x-amz-date\n" + payloadHash
	scope := dateStamp + "/" + region + "/s3/aws4_request"
	stringToSign := "AWS4-HMAC-SHA256\n" + amz + "\n" + scope + "\n" + sha256Hex([]byte(canonical))
	k := hmacSHA256([]byte("AWS4"+sk), []byte(dateStamp))
	k = hmacSHA256(k, []byte(region))
	k = hmacSHA256(k, []byte("s3"))
	k = hmacSHA256(k, []byte("aws4_request"))
	sig := hex.EncodeToString(hmacSHA256(k, []byte(stringToSign)))
	return sigHeaders{
		authorization: "AWS4-HMAC-SHA256 Credential=" + ak + "/" + scope +
			", SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=" + sig,
		xAmzDate:          amz,
		xAmzContentSHA256: payloadHash,
	}
}

func signRequest(method, path string, query [][2]string, payload []byte, now float64) sigHeaders {
	return signRequestWith(OSSAK, OSSSK, OSSRegion, method, path, query, payload, now)
}

// ---- S3 operations ----

type ossEntry struct {
	name     string // file name relative to the listed prefix
	key      string
	size     int64
	modified string
}

// ossDo performs a signed request and returns the raw Response.
func ossDo(method, path string, query [][2]string, payload []byte) js.Value {
	sig := signRequest(method, path, query, payload, nowMs())
	qs := canonicalQuery(query)
	url := "https://" + OSSEndpoint + path
	if qs != "" {
		url += "?" + qs
	}
	var body js.Value
	if payload != nil {
		u8 := js.Global().Get("Uint8Array").New(len(payload))
		for i, b := range payload {
			u8.SetIndex(i, b)
		}
		body = u8
	}
	opts := js.Global().Get("Object").New()
	opts.Set("method", method)
	if body.Truthy() {
		opts.Set("body", body)
	}
	headers := js.Global().Get("Object").New()
	headers.Set("x-amz-date", sig.xAmzDate)
	headers.Set("x-amz-content-sha256", sig.xAmzContentSHA256)
	headers.Set("Authorization", sig.authorization)
	opts.Set("headers", headers)
	return await(js.Global().Call("fetch", url, opts))
}

func ossGet(key string) (string, error) {
	path := "/" + OSSBucket + "/" + awsURIEncode(key, false)
	resp := ossDo("GET", path, nil, nil)
	switch resp.Get("status").Int() {
	case 200:
		return await(resp.Call("text")).String(), nil
	case 404:
		return "", errString("文件不存在")
	default:
		return "", errString("下载失败 (HTTP " + itoa(resp.Get("status").Int()) + ")")
	}
}

func ossDelete(key string) error {
	path := "/" + OSSBucket + "/" + awsURIEncode(key, false)
	resp := ossDo("DELETE", path, nil, nil)
	switch s := resp.Get("status").Int(); {
	case s == 204 || s == 200:
		return nil
	case s == 404:
		return errString("文件不存在")
	default:
		return errString("删除失败 (HTTP " + itoa(s) + ")")
	}
}

func ossExists(key string) bool {
	_, err := ossGet(key)
	return err == nil
}

func ossPut(key, content string) error {
	path := "/" + OSSBucket + "/" + awsURIEncode(key, false)
	resp := ossDo("PUT", path, nil, []byte(content))
	if resp.Get("ok").Bool() {
		return nil
	}
	return errString("保存失败 (HTTP " + itoa(resp.Get("status").Int()) + ")")
}

// listBucketResult models the ListObjectsV2 response.
type listBucketResult struct {
	XMLName     xml.Name `xml:"ListBucketResult"`
	IsTruncated bool     `xml:"IsTruncated"`
	NextToken   string   `xml:"NextContinuationToken"`
	Contents    []struct {
		Key          string `xml:"Key"`
		Size         int64  `xml:"Size"`
		LastModified string `xml:"LastModified"`
	} `xml:"Contents"`
}

// listAllEntriesUnder lists ALL objects under prefix, following pagination.
func listAllEntriesUnder(prefix string) ([]ossEntry, error) {
	var out []ossEntry
	token := ""
	for {
		query := [][2]string{{"list-type", "2"}, {"max-keys", "1000"}, {"prefix", prefix}}
		if token != "" {
			query = append(query, [2]string{"continuation-token", token})
		}
		resp := ossDo("GET", "/"+OSSBucket, query, nil)
		if !resp.Get("ok").Bool() {
			return nil, errString("列举失败 (HTTP " + itoa(resp.Get("status").Int()) + ")")
		}
		text := await(resp.Call("text")).String()
		var lr listBucketResult
		if err := xml.Unmarshal([]byte(text), &lr); err != nil {
			return nil, errString("目录解析失败: " + err.Error())
		}
		for _, c := range lr.Contents {
			mod := c.LastModified
			if len(mod) >= 10 {
				mod = mod[:10]
			}
			out = append(out, ossEntry{name: c.Key, key: c.Key, size: c.Size, modified: mod})
		}
		if lr.IsTruncated {
			token = lr.NextToken
		} else {
			break
		}
	}
	return out, nil
}

// ---- tree ----

type treeNode struct {
	name   string
	prefix string
	open   bool
	dirs   []*treeNode
	files  []ossEntry
}

func (n *treeNode) find(prefix string) *treeNode {
	if n.prefix == prefix {
		return n
	}
	for _, d := range n.dirs {
		if r := d.find(prefix); r != nil {
			return r
		}
	}
	return nil
}

// buildTree builds the directory tree from a flat listing. Expansion state
// carries over from the previous tree (matched by prefix).
func buildTree(entries []ossEntry, old *treeNode) *treeNode {
	root := &treeNode{}
	for _, e := range entries {
		parts := strings.Split(e.key, "/")
		node := root
		acc := ""
		for _, seg := range parts[:len(parts)-1] {
			if seg == "" {
				continue
			}
			acc += seg + "/"
			var child *treeNode
			for _, d := range node.dirs {
				if d.prefix == acc {
					child = d
					break
				}
			}
			if child == nil {
				open := false
				if old != nil {
					if o := old.find(acc); o != nil {
						open = o.open
					}
				}
				child = &treeNode{name: seg, prefix: acc, open: open}
				node.dirs = append(node.dirs, child)
			}
			node = child
		}
		last := parts[len(parts)-1]
		if last == "" {
			// a folder marker object — the dir itself was created above
		} else {
			node.files = append(node.files, ossEntry{name: last, key: e.key, size: e.size, modified: e.modified})
		}
	}
	sortTree(root)
	return root
}

func sortTree(n *treeNode) {
	sort.Slice(n.dirs, func(i, j int) bool { return n.dirs[i].name < n.dirs[j].name })
	sort.Slice(n.files, func(i, j int) bool { return n.files[i].name < n.files[j].name })
	for _, d := range n.dirs {
		sortTree(d)
	}
}

// ---- page state ----

type ossPage struct {
	tree        *treeNode
	current     string // "" = none
	currentName string
	dirty       bool
	suppress    float64
	status      string
	editor      jsEditor
	hasEditor   bool
}

var oss = &ossPage{}

func sanitizeName(raw string) (string, bool) {
	var b strings.Builder
	n := 0
	for _, c := range raw {
		switch c {
		case '/', '\\', ':', '*', '?', '"', '<', '>', '|', '\n', '\r':
			continue
		}
		b.WriteRune(c)
		n++
		if n >= 120 {
			break
		}
	}
	name := strings.TrimSpace(b.String())
	if name == "" || name == "." || name == ".." {
		return "", false
	}
	return name, true
}

// refreshTree reloads the whole bucket into the tree, preserving expansion.
func refreshTree() {
	oss.status = "加载目录…"
	renderOssStatus()
	go func() {
		entries, err := listAllEntriesUnder("")
		if err != nil {
			oss.status = err.Error()
			renderOssStatus()
			return
		}
		oss.tree = buildTree(entries, oss.tree)
		oss.status = ""
		renderOssTree()
		renderOssStatus()
	}()
}

// copyPrefix copies everything under oldPrefix to newPrefix, then deletes the
// originals (S3 has no rename — GET+PUT per object).
func copyPrefix(oldPrefix, newPrefix string) error {
	entries, err := listAllEntriesUnder(oldPrefix)
	if err != nil {
		return err
	}
	total := len(entries)
	for i, e := range entries {
		rest := strings.TrimPrefix(e.key, oldPrefix)
		target := newPrefix + rest
		oss.status = "重命名中 (" + itoa(i+1) + "/" + itoa(total) + ")…"
		renderOssStatus()
		content, err := ossGet(e.key)
		if err != nil {
			return err
		}
		if err := ossPut(target, content); err != nil {
			return err
		}
		if err := ossDelete(e.key); err != nil {
			return err
		}
	}
	if total == 0 {
		// folder had no objects at all — leave a marker so it still exists
		if err := ossPut(newPrefix, ""); err != nil {
			return err
		}
	}
	return nil
}

func renameFileKey(oldKey, newKey string) error {
	content, err := ossGet(oldKey)
	if err != nil {
		return err
	}
	if err := ossPut(newKey, content); err != nil {
		return err
	}
	return ossDelete(oldKey)
}

func openOssFile(key, name string) {
	oss.status = "载入 " + name + "…"
	renderOssStatus()
	go func() {
		text, err := ossGet(key)
		if err != nil {
			oss.status = err.Error()
			renderOssStatus()
			return
		}
		oss.suppress = nowMs() + 500
		oss.editor.setValue(text)
		oss.current = key
		oss.currentName = name
		oss.dirty = false
		doc().Set("title", name+" · 文件库")
		oss.status = "已载入 " + name
		renderOssTree()
		renderOssStatus()
		renderOssFilebar()
	}()
}

func saveOssCurrent() {
	if oss.current == "" {
		return
	}
	content := oss.editor.getValue()
	oss.status = "保存中…"
	renderOssStatus()
	go func() {
		if err := ossPut(oss.current, content); err != nil {
			oss.status = err.Error()
			renderOssStatus()
			return
		}
		oss.dirty = false
		oss.status = "已保存 " + currentTime()
		renderOssStatus()
		renderOssFilebar()
		refreshTree()
	}()
}

// fixCurrentAfterMove rebases the open-file state after a rename, or clears
// it when the file was deleted.
func fixCurrentAfterMove(old string, newPrefix *string) {
	if oss.current == "" {
		return
	}
	cur := oss.current
	affected := cur == old
	if strings.HasSuffix(old, "/") {
		affected = strings.HasPrefix(cur, old)
	}
	if !affected {
		return
	}
	if newPrefix != nil {
		rest := strings.TrimPrefix(cur, old)
		newKey := *newPrefix + rest
		oss.current = newKey
		parts := strings.Split(newKey, "/")
		oss.currentName = parts[len(parts)-1]
	} else {
		oss.current = ""
		oss.currentName = ""
		oss.dirty = false
		oss.editor.setValue("")
		doc().Set("title", "文件库")
	}
	renderOssFilebar()
}

func createIn(dirPrefix string, isDir bool) {
	what := "文件"
	if isDir {
		what = "文件夹"
	}
	raw, ok := prompt("在 " + dirPrefix + " 下新建" + what + "：")
	if !ok {
		return
	}
	name, ok := sanitizeName(raw)
	if !ok {
		return
	}
	var key, content, finalName string
	if isDir {
		key = dirPrefix + name + "/"
		finalName = name
	} else {
		finalName = name
		if !strings.Contains(name, ".") {
			finalName = name + ".md"
		}
		key = dirPrefix + finalName
		content = "# " + finalName + "\n"
	}
	oss.status = "创建 " + finalName + "…"
	renderOssStatus()
	go func() {
		if !isDir && ossExists(key) &&
			!confirm(finalName+" 已存在，覆盖它？") {
			oss.status = "已取消"
			renderOssStatus()
			return
		}
		if err := ossPut(key, content); err != nil {
			oss.status = err.Error()
			renderOssStatus()
			return
		}
		oss.status = "已创建 " + finalName
		renderOssStatus()
		refreshTree()
		if !isDir {
			openOssFile(key, finalName)
		}
	}()
}

func renameEntry(path string, isDir bool) {
	trimmed := strings.TrimSuffix(path, "/")
	oldName := trimmed
	if i := strings.LastIndex(trimmed, "/"); i >= 0 {
		oldName = trimmed[i+1:]
	}
	raw, ok := prompt("将 " + oldName + " 重命名为：")
	if !ok {
		return
	}
	name, ok := sanitizeName(raw)
	if !ok {
		return
	}
	parent := ""
	if i := strings.LastIndex(trimmed, "/"); i >= 0 {
		parent = trimmed[:i+1]
	}
	var newPath, confirmMsg string
	if isDir {
		newPath = parent + name + "/"
		confirmMsg = "文件夹内的所有文件将移动到 " + parent + name + "/，继续？"
	} else {
		finalName := name
		if !strings.Contains(name, ".") {
			finalName = name + ".md"
		}
		newPath = parent + finalName
		confirmMsg = "确定重命名为 " + finalName + "？"
	}
	if newPath == path {
		return
	}
	if !confirm(confirmMsg) {
		return
	}
	oss.status = "重命名 " + oldName + "…"
	renderOssStatus()
	go func() {
		var err error
		if isDir {
			err = copyPrefix(path, newPath)
		} else {
			err = renameFileKey(path, newPath)
		}
		if err != nil {
			oss.status = err.Error()
			renderOssStatus()
			return
		}
		np := newPath
		fixCurrentAfterMove(path, &np)
		oss.status = "已重命名为 " + name
		renderOssStatus()
		refreshTree()
	}()
}

func deleteEntry(path string, isDir bool) {
	trimmed := strings.TrimSuffix(path, "/")
	name := trimmed
	if i := strings.LastIndex(trimmed, "/"); i >= 0 {
		name = trimmed[i+1:]
	}
	hint := ""
	if isDir {
		hint = "（仅限空文件夹）"
	}
	if !confirm("删除 " + name + hint + "？此操作不可恢复。") {
		return
	}
	oss.status = "删除 " + name + "…"
	renderOssStatus()
	go func() {
		var err error
		if isDir {
			// only empty folders: anything besides the marker blocks it
			entries, lerr := listAllEntriesUnder(path)
			if lerr != nil {
				err = lerr
			} else {
				nonMarker := false
				for _, e := range entries {
					if e.key != path {
						nonMarker = true
						break
					}
				}
				if nonMarker {
					err = errString("文件夹非空，请先删除其中的文件")
				} else {
					err = ossDelete(path)
				}
			}
		} else {
			err = ossDelete(path)
		}
		if err != nil {
			oss.status = err.Error()
			renderOssStatus()
			return
		}
		fixCurrentAfterMove(path, nil)
		oss.status = "已删除 " + name
		renderOssStatus()
		refreshTree()
	}()
}

// ---- page rendering ----

func renderOssStatus() {
	e := qs(".oss-status")
	if e.Truthy() {
		e.Set("textContent", oss.status)
	}
}

func renderOssFilebar() {
	e := qs(".oss-filebar")
	if !e.Truthy() {
		return
	}
	html := escapeHTML(oss.currentName)
	if oss.dirty && oss.current != "" {
		html += `<span class="oss-dirty">● 未保存</span>`
	}
	e.Set("innerHTML", html)
}

func renderOssTree() {
	mount := qs(".oss-tree")
	if !mount.Truthy() {
		return
	}
	mount.Set("innerHTML", "")
	if oss.tree == nil || (len(oss.tree.dirs) == 0 && len(oss.tree.files) == 0) {
		mount.Call("appendChild", el("div", map[string]any{"class": "oss-hint"}, "（空桶，点上方按钮新建）"))
		return
	}
	renderOssTreeNode(mount, oss.tree)
}

func renderOssTreeNode(mount jsValue, node *treeNode) {
	for _, d := range node.dirs {
		renderOssDir(mount, d)
	}
	for _, f := range node.files {
		renderOssFileRow(mount, f)
	}
}

func renderOssDir(mount jsValue, d *treeNode) {
	dir := d
	wrap := el("div", map[string]any{"class": "oss-tdir"})
	row := el("div", map[string]any{"class": "oss-trow"})
	on(row, "click", func(e jsValue) { dir.open = !dir.open; renderOssTree() })
	chev := el("span", map[string]any{"class": "oss-chev"}, "▸")
	if dir.open {
		chev.Get("classList").Call("add", "oss-chev-open")
	}
	icon := "📁"
	if dir.open {
		icon = "📂"
	}
	row.Call("appendChild", chev)
	row.Call("appendChild", el("span", map[string]any{"class": "oss-ticon"}, icon))
	row.Call("appendChild", el("span", map[string]any{"class": "oss-tname"}, dir.name))
	act := el("span", map[string]any{"class": "oss-tact"})
	b1 := el("button", map[string]any{"type": "button", "title": "在此新建文件"}, "＋📄")
	on(b1, "click", func(e jsValue) { e.Call("stopPropagation"); createIn(dir.prefix, false) })
	b2 := el("button", map[string]any{"type": "button", "title": "在此新建文件夹"}, "＋📁")
	on(b2, "click", func(e jsValue) { e.Call("stopPropagation"); createIn(dir.prefix, true) })
	b3 := el("button", map[string]any{"type": "button", "title": "重命名文件夹"}, "✏️")
	on(b3, "click", func(e jsValue) { e.Call("stopPropagation"); renameEntry(dir.prefix, true) })
	b4 := el("button", map[string]any{"type": "button", "title": "删除文件夹（仅限空文件夹）"}, "🗑")
	on(b4, "click", func(e jsValue) { e.Call("stopPropagation"); deleteEntry(dir.prefix, true) })
	for _, b := range []jsValue{b1, b2, b3, b4} {
		act.Call("appendChild", b)
	}
	row.Call("appendChild", act)
	wrap.Call("appendChild", row)
	if dir.open {
		kids := el("div", map[string]any{"class": "oss-tkids"})
		renderOssTreeNode(kids, dir)
		wrap.Call("appendChild", kids)
	}
	mount.Call("appendChild", wrap)
}

func renderOssFileRow(mount jsValue, f ossEntry) {
	file := f
	row := el("div", map[string]any{"class": "oss-trow oss-tfile"})
	if oss.current == file.key {
		row.Get("classList").Call("add", "oss-active")
	}
	open := el("button", map[string]any{"type": "button", "class": "oss-open"})
	open.Call("appendChild", el("span", map[string]any{"class": "oss-ticon"}, "📄"))
	open.Call("appendChild", el("span", map[string]any{"class": "oss-tname"}, file.name))
	on(open, "click", func(e jsValue) { openOssFile(file.key, file.name) })
	act := el("span", map[string]any{"class": "oss-tact"})
	b3 := el("button", map[string]any{"type": "button", "title": "重命名文件"}, "✏️")
	on(b3, "click", func(e jsValue) { renameEntry(file.key, false) })
	b4 := el("button", map[string]any{"type": "button", "title": "删除文件"}, "🗑")
	on(b4, "click", func(e jsValue) { deleteEntry(file.key, false) })
	act.Call("appendChild", b3)
	act.Call("appendChild", b4)
	row.Call("appendChild", open)
	row.Call("appendChild", act)
	mount.Call("appendChild", row)
}

// renderOssPage mounts the /oss file library page.
func renderOssPage() {
	root := qs("#app")
	root.Set("innerHTML", "")
	page := el("div", map[string]any{"class": "oss-page"})
	top := el("div", map[string]any{"class": "oss-top"})
	top.Call("appendChild", el("a", map[string]any{"class": "oss-home", "href": BASE_PATH + "/", "title": "返回便签页面"}, "📝 便签"))
	top.Call("appendChild", el("span", map[string]any{"class": "oss-brand"}, "🗂 文件库"))
	top.Call("appendChild", el("span", map[string]any{"class": "oss-spacer"}))
	refreshBtn := el("button", map[string]any{"type": "button", "class": "tool", "title": "重新加载目录"}, "🔄")
	on(refreshBtn, "click", func(e jsValue) { refreshTree() })
	saveBtn := el("button", map[string]any{"type": "button", "class": "tool oss-save", "title": "保存回对象存储"}, "💾 保存")
	saveBtn.Set("disabled", true)
	on(saveBtn, "click", func(e jsValue) { saveOssCurrent() })
	status := el("span", map[string]any{"class": "oss-status"})
	top.Call("appendChild", refreshBtn)
	top.Call("appendChild", saveBtn)
	top.Call("appendChild", status)

	body_ := el("div", map[string]any{"class": "oss-body"})
	side := el("aside", map[string]any{"class": "oss-side"})
	tools := el("div", map[string]any{"class": "oss-tools"})
	newFile := el("button", map[string]any{"type": "button", "class": "tool", "title": "在根目录新建 Markdown 文件"}, "＋ 文件")
	on(newFile, "click", func(e jsValue) { createIn("", false) })
	newDir := el("button", map[string]any{"type": "button", "class": "tool", "title": "在根目录新建文件夹"}, "＋ 文件夹")
	on(newDir, "click", func(e jsValue) { createIn("", true) })
	tools.Call("appendChild", newFile)
	tools.Call("appendChild", newDir)
	tree := el("div", map[string]any{"class": "oss-tree"})
	side.Call("appendChild", tools)
	side.Call("appendChild", tree)
	main_ := el("main", map[string]any{"class": "oss-main"})
	filebar := el("div", map[string]any{"class": "oss-filebar"})
	editorMount := el("div", map[string]any{"class": "oss-editor"})
	main_.Call("appendChild", filebar)
	main_.Call("appendChild", editorMount)
	body_.Call("appendChild", side)
	body_.Call("appendChild", main_)
	page.Call("appendChild", top)
	page.Call("appendChild", body_)
	root.Call("appendChild", page)

	doc().Set("title", "文件库")
	initOssEditor(editorMount)
	refreshTree()
}

func initOssEditor(mount jsValue) {
	if oss.hasEditor {
		return
	}
	dark := js.Global().Call("matchMedia", "(prefers-color-scheme: dark)").Get("matches").Bool()
	opts := js.Global().Get("Object").New()
	opts.Set("value", "")
	opts.Set("mode", "wysiwyg")
	opts.Set("lang", "zh_CN")
	opts.Set("cdn", BASE_PATH+"/vditor")
	preview := js.Global().Get("Object").New()
	if dark {
		opts.Set("theme", "dark")
		preview.Set("theme", "dark")
	} else {
		opts.Set("theme", "classic")
		preview.Set("theme", "light")
	}
	opts.Set("preview", preview)
	opts.Set("placeholder", "从左侧选择一个 Markdown 文件开始编辑，编辑后点右上角保存…")
	cache := js.Global().Get("Object").New()
	cache.Set("enable", false)
	opts.Set("cache", cache)
	inputCb := js.FuncOf(func(_ js.Value, _ []js.Value) any {
		// Vditor fires input asynchronously right after set_value — that echo
		// is not a user edit
		if nowMs() < oss.suppress {
			return nil
		}
		oss.dirty = true
		renderOssFilebar()
		renderOssSaveBtn()
		return nil
	})
	keep(inputCb)
	opts.Set("input", inputCb)
	oss.editor.cbs = append(oss.editor.cbs, inputCb)
	toolbar := js.Global().Get("Array").New()
	for _, t := range []string{
		"headings", "bold", "italic", "strike", "|",
		"list", "ordered-list", "check", "quote", "line", "code", "inline-code", "link",
		"table", "|", "undo", "redo", "|", "edit-mode", "preview",
	} {
		toolbar.Call("push", t)
	}
	opts.Set("toolbar", toolbar)
	oss.editor.v = js.Global().Get("Vditor").New(mount, opts)
	oss.hasEditor = true
}

func renderOssSaveBtn() {
	btn := qs(".oss-save")
	if btn.Truthy() {
		btn.Set("disabled", !(oss.dirty && oss.current != ""))
	}
}
