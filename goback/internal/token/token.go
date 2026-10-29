// Package token ports the Rust worker's token.rs: CJK tokenization for
// full-text search and POS annotation, backed by gojieba (cppjieba's Go
// binding — the same dictionary/algorithm lineage as jieba-rs) for Chinese
// and kagome with the IPADIC dictionary (same system dictionary family as
// Vibrato/lindera) for Japanese.
//
// As in the Rust version, both segmenters run over the whole text and their
// token sets are unioned: han characters are shared by both languages, so
// script-based routing can't tell a kanji-only Japanese sentence from
// Chinese.
package token

import (
	"cmp"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/ikawaha/kagome-dict/ipa"
	"github.com/ikawaha/kagome/v2/tokenizer"
	"github.com/yanyiwu/gojieba"
)

const (
	// POSDocMaxChars mirrors the Rust POS_DOC_MAX_CHARS: very long notes are
	// truncated so annotated documents stay a reasonable size.
	POSDocMaxChars = 30000
	// IndexMaxChars mirrors INDEX_MAX_CHARS: only the head of very long
	// notes is indexed.
	IndexMaxChars  = 30000
	segChunkChars  = 4000
)

// Analyzer is the shared segmenter pair. Construct one per process with New.
type Analyzer struct {
	jieba *gojieba.Jieba
	ja    *tokenizer.Tokenizer
}

func New() *Analyzer {
	d, err := tokenizer.New(ipa.Dict())
	if err != nil {
		// the embedded dictionary is compiled in; failure means a corrupt
		// build, not a runtime condition
		panic("kagome ipadic init: " + err.Error())
	}
	return &Analyzer{jieba: gojieba.NewJieba(), ja: d}
}

// Close releases the cppjieba segmenter (its finalizer also covers this, but
// be explicit).
func (a *Analyzer) Close() { a.jieba.Free() }

// Tok is one POS-tagged span: word, normalized category, char offsets.
type Tok struct {
	W   string
	Tag string // normalized category key (n/v/a/d/…)
	S   int    // char offset start
	E   int    // char offset end
}

// ---- text chunking ----

func isBreak(r rune) bool {
	return strings.ContainsRune("\n。！？;；.!｡・", r)
}

// forEachChunk splits long text into sentence-boundary-aligned chunks and
// calls f(charBase, chunk). The segmenters' lattice memory grows with input
// length, so very long notes are segmented in slices.
func forEachChunk(text string, maxChars int, f func(base int, chunk string)) {
	runes := []rune(text)
	if len(runes) <= maxChars {
		f(0, text)
		return
	}
	start := 0 // char index of the current chunk start
	breakAt := -1
	for i := 0; i < len(runes); i++ {
		if isBreak(runes[i]) {
			breakAt = i + 1
		}
		if i-start+1 >= maxChars {
			end := breakAt
			if end <= start {
				end = i // current char excluded, like the Rust fallback
			}
			f(start, string(runes[start:end]))
			start = end
			breakAt = -1
		}
	}
	if start < len(runes) {
		f(start, string(runes[start:]))
	}
}

// ---- token filtering ----

func isCJK(r rune) bool {
	return (r >= 0x4e00 && r <= 0x9fff) ||
		(r >= 0x3040 && r <= 0x30ff) ||
		(r >= 0xf900 && r <= 0xfaff)
}

func isKana(r rune) bool { return r >= 0x3040 && r <= 0x30ff }

// HasKana reports whether the token contains kana — i.e. it plausibly
// belongs to a Japanese passage. The IPADIC segmenter mangles pure-han
// Chinese text, so annotation keeps only its kana-bearing tokens and lets
// jieba own pure-han runs.
func HasKana(w string) bool {
	return strings.IndexFunc(w, isKana) >= 0
}

func noLetters(w string) bool {
	return strings.IndexFunc(w, func(r rune) bool { return isAlnum(r) }) < 0
}

// isAlnum approximates Rust's char::is_alphanumeric plus the explicit '_'
// kept by push_token: ASCII alnum, CJK ideographs, kana, and the other
// Unicode scripts that realistically appear in notes.
func isAlnum(r rune) bool {
	if r == '_' ||
		(r >= '0' && r <= '9') || (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') {
		return true
	}
	return isCJK(r) || unicodeLetter(r)
}

func unicodeLetter(r rune) bool {
	return (r >= 0x3040 && r <= 0x30ff) || // hiragana + katakana
		(r >= 0x3400 && r <= 0x4dbf) || // CJK ext A
		(r >= 0x20000 && r <= 0x2a6df) || // CJK ext B
		(r >= 0xf900 && r <= 0xfaff) || // compat ideographs
		(r >= 0xac00 && r <= 0xd7af) || // hangul syllables
		(r >= 0x370 && r <= 0x3ff) || // greek
		(r >= 0x400 && r <= 0x4ff) || // cyrillic
		(r >= 0x590 && r <= 0x5ff) || // hebrew
		(r >= 0x600 && r <= 0x6ff) || // arabic
		(r >= 0x0e00 && r <= 0x0e7f) || // thai
		(r >= 0xff00 && r <= 0xffef) || // fullwidth/halfwidth forms
		r >= 0x1f300 // emoji & supplementary letters
}

func allKana(w string) bool {
	if w == "" {
		return false
	}
	for _, r := range w {
		if !isKana(r) {
			return false
		}
	}
	return true
}

func hasMDMeta(w string) bool {
	return strings.ContainsAny(w, "*_`#[]()<>|~!")
}

// pushToken cleans a raw segment into index tokens: keep word characters
// (letters incl. CJK, digits, underscore), drop punctuation (this also
// strips tsquery syntax chars), lowercase. Single CJK characters are added
// as redundant single-char tokens so index and query always agree even when
// the two segmenters' cuts differ (e.g. 故宫 as 故/宫 vs whole).
func pushToken(out []string, raw string) []string {
	var cleaned []rune
	for _, r := range raw {
		if isAlnum(r) {
			cleaned = append(cleaned, r)
		}
	}
	if len(cleaned) == 0 {
		return out
	}
	word := strings.ToLower(string(cleaned))
	out = append(out, word)
	for _, r := range word {
		if isCJK(r) {
			out = append(out, string(r))
		}
	}
	return out
}

// ---- Chinese ----

func (a *Analyzer) zhTokenize(text string, out []string) []string {
	for _, t := range a.jieba.Cut(text, false) {
		out = pushToken(out, t)
	}
	return out
}

// ZhTag segments Chinese with POS tags and char offsets.
func (a *Analyzer) ZhTag(text string) []Tok { return a.zhTag(text, false) }

// ZhTagHMM is the HMM-enabled variant used by the analyzer's duplicate
// detector.
func (a *Analyzer) ZhTagHMM(text string) []Tok { return a.zhTag(text, true) }

func (a *Analyzer) zhTag(text string, hmm bool) []Tok {
	var out []Tok
	forEachChunk(text, segChunkChars, func(base int, chunk string) {
		pos := 0 // byte offset cursor
		for _, wt := range a.jieba.Tag(chunk) {
			word, tag := splitTag(wt)
			if word == "" {
				continue
			}
			startByte := pos
			if idx := strings.Index(chunk[pos:], word); idx >= 0 {
				startByte = pos + idx
			}
			endByte := startByte + len(word)
			out = append(out, Tok{
				W:   word,
				Tag: correctZhCat(word, zhCat(tag)),
				S:   base + utf8.RuneCountInString(chunk[:startByte]),
				E:   base + utf8.RuneCountInString(chunk[:endByte]),
			})
			pos = endByte
		}
	})
	return out
}

// splitTag splits gojieba's "word/tag" formatting at the last '/'.
func splitTag(wt string) (string, string) {
	i := strings.LastIndex(wt, "/")
	if i < 0 {
		return wt, ""
	}
	return wt[:i], wt[i+1:]
}

// zhCat normalizes jieba's dictionary tags to display categories.
func zhCat(tag string) string {
	switch tag {
	case "z", "zg": // state words (很/太/最/都…) read as adverbs
		return "d"
	case "b", "bl": // distinguisher words read as adjectives
		return "a"
	case "f": // locatives get their own bucket
		return "f"
	case "t", "tg": // time words → noun
		return "n"
	case "i": // HMM-discovered neologism — noun is the best guess
		return "n"
	}
	if tag == "" {
		return "x"
	}
	switch tag[0] {
	case 'n':
		return "n"
	case 'v':
		return "v"
	case 'a':
		return "a"
	case 'd':
		return "d"
	case 'u':
		return "u"
	case 'r':
		return "r"
	case 'm':
		return "m"
	case 'q':
		return "q"
	case 'c':
		return "c"
	case 'p':
		return "p"
	}
	return "x"
}

// correctZhCat patches frequent words the bundled dictionary mislabels
// (美丽 as a place name ns, 好吃 as a verb…).
func correctZhCat(word, cat string) string {
	switch word {
	case "美丽", "漂亮", "好看", "英俊", "可爱", "聪明", "优秀", "精彩", "有趣",
		"高兴", "快乐", "开心", "难过", "伤心", "生气", "害怕", "热闹", "安静",
		"干净", "新鲜", "方便", "重要", "危险", "安全", "温暖", "凉快", "舒服",
		"难受", "好吃", "好喝", "好玩", "容易", "简单", "困难", "年轻":
		return "a"
	case "正在", "正", "刚刚", "常常", "经常", "时常", "大声", "小声", "轻声":
		return "d"
	}
	return cat
}

// ---- Japanese ----

// JaTokens segments Japanese, keeping the first two IPADIC POS feature
// fields as the tag, with char offsets.
func (a *Analyzer) JaTokens(text string) []Tok {
	var out []Tok
	forEachChunk(text, segChunkChars, func(base int, chunk string) {
		for _, t := range a.ja.Tokenize(chunk) {
			if t.Class == tokenizer.DUMMY {
				continue // BOS/EOS sentinels — vibrato's token stream excludes these too
			}
			feat := t.Features()
			n := min(len(feat), 2)
			pos := strings.Join(feat[:n], ",")
			out = append(out, Tok{
				W:   t.Surface,
				Tag: pos,
				S:   base + utf8.RuneCountInString(chunk[:t.Start]),
				E:   base + utf8.RuneCountInString(chunk[:t.End]),
			})
		}
	})
	return out
}

// jaCat maps the first IPADIC POS field to a normalized category.
func jaCat(pos string) (string, bool) {
	head := pos
	if i := strings.Index(pos, ","); i >= 0 {
		head = pos[:i]
	}
	switch head {
	case "名詞":
		if strings.Contains(pos, "代名詞") {
			return "r", true
		}
		return "n", true
	case "動詞":
		return "v", true
	case "形容詞", "連体詞":
		return "a", true
	case "副詞":
		return "d", true
	case "助詞":
		return "u", true
	case "助動詞":
		return "aux", true
	case "接続詞":
		return "c", true
	}
	return "", false
}

// ---- annotated markdown ----

const poswOpen = `<span class="posw cat-`

// stripExistingPosw removes `<span class="posw cat-*">…</span>` wrappers from
// a previous annotation pass so re-generating is idempotent.
func stripExistingPosw(md string) string {
	var b strings.Builder
	rest := md
	for {
		i := strings.Index(rest, poswOpen)
		if i < 0 {
			b.WriteString(rest)
			break
		}
		b.WriteString(rest[:i])
		gt := strings.Index(rest[i:], ">")
		if gt < 0 {
			break
		}
		after := rest[i+gt+1:]
		if c := strings.Index(after, "</span>"); c >= 0 {
			b.WriteString(after[:c])
			rest = after[c+len("</span>"):]
		} else {
			rest = after
		}
	}
	return b.String()
}

// codeRanges returns char ranges covered by code (fenced blocks and `inline`
// spans) — tokens inside must not be annotated.
func codeRanges(text string) [][2]int {
	var ranges [][2]int
	inFence := false
	pos := 0
	for _, line := range strings.SplitAfter(text, "\n") {
		chars := utf8.RuneCountInString(line)
		trimmed := strings.TrimLeft(line, " \t")
		if strings.HasPrefix(trimmed, "```") || strings.HasPrefix(trimmed, "~~~") {
			ranges = append(ranges, [2]int{pos, pos + chars})
			inFence = !inFence
			pos += chars
			continue
		}
		if inFence {
			ranges = append(ranges, [2]int{pos, pos + chars})
			pos += chars
			continue
		}
		// inline code: toggle on each backtick run (assumes well-formed input)
		inCode := false
		seg := pos
		for _, part := range strings.Split(line, "`") {
			n := utf8.RuneCountInString(part)
			if inCode {
				ranges = append(ranges, [2]int{seg, seg + n})
			}
			inCode = !inCode
			seg += n + 1 // +1 for the consumed backtick
		}
		pos += chars
	}
	return ranges
}

func inRanges(ranges [][2]int, s, e int) bool {
	for _, r := range ranges {
		if s < r[1] && e > r[0] {
			return true
		}
	}
	return false
}

// span is one annotated char range with its category.
type span struct {
	s, e int
	cat  string
}

// AnnotateMarkdown re-renders md with every (Chinese + Japanese) word
// wrapped in a `<span class="posw cat-*">` tag. Overlapping tokenizations
// are deduped so the produced markdown/HTML stays properly nested.
func (a *Analyzer) AnnotateMarkdown(md string) string {
	src := stripExistingPosw(md)
	mask := codeRanges(src)

	var toks []span
	for _, t := range a.ZhTag(src) {
		if t.S >= POSDocMaxChars {
			break
		}
		if noLetters(t.W) || allKana(t.W) || hasMDMeta(t.W) || inRanges(mask, t.S, t.E) {
			continue
		}
		toks = append(toks, span{t.S, t.E, t.Tag})
	}
	for _, t := range a.JaTokens(src) {
		if t.S >= POSDocMaxChars {
			break
		}
		cat, ok := jaCat(t.Tag)
		if !ok {
			continue
		}
		if !HasKana(t.W) || noLetters(t.W) || hasMDMeta(t.W) || inRanges(mask, t.S, t.E) {
			continue // pure-han tokens belong to the Chinese segmenter
		}
		toks = append(toks, span{t.S, t.E, cat})
	}
	// dedupe overlaps: sort by start asc, end desc; greedily take non-overlapping spans
	slices.SortFunc(toks, func(x, y span) int {
		if c := cmp.Compare(x.s, y.s); c != 0 {
			return c
		}
		return cmp.Compare(y.e, x.e)
	})
	var merged []span
	lastEnd := 0
	for _, t := range toks {
		if t.s >= lastEnd {
			lastEnd = t.e
			merged = append(merged, t)
		}
	}

	chars := []rune(src)
	var out strings.Builder
	out.Grow(len(src) + len(merged)*42)
	cursor := 0
	for _, t := range merged {
		if t.s < cursor || t.e > len(chars) {
			continue
		}
		out.WriteString(string(chars[cursor:t.s]))
		out.WriteString(`<span class="posw cat-`)
		out.WriteString(t.cat)
		out.WriteString(`">`)
		out.WriteString(string(chars[t.s:t.e]))
		out.WriteString("</span>")
		cursor = t.e
	}
	out.WriteString(string(chars[cursor:]))
	return out.String()
}

// Tokenize tokenizes with jieba + kagome, unions the token sets and dedupes.
// It is safe for concurrent use (both segmenters are read-only after New).
func (a *Analyzer) Tokenize(text string) []string {
	var out []string
	forEachChunk(text, segChunkChars, func(_ int, chunk string) {
		out = a.zhTokenize(chunk, out)
	})
	forEachChunk(text, segChunkChars, func(_ int, chunk string) {
		for _, t := range a.ja.Tokenize(chunk) {
			out = pushToken(out, t.Surface)
		}
	})
	slices.Sort(out)
	return slices.Compact(out)
}
