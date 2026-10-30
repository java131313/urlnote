//go:build !wasm

package token

import (
	"strings"
	"unicode/utf8"

	"github.com/ikawaha/kagome-dict/ipa"
	"github.com/ikawaha/kagome/v2/tokenizer"
	"github.com/yanyiwu/gojieba"
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

// jaTokenize appends Japanese index tokens.
func (a *Analyzer) jaTokenize(chunk string, out []string) []string {
	for _, t := range a.ja.Tokenize(chunk) {
		out = pushToken(out, t.Surface)
	}
	return out
}

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
