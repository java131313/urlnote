//go:build wasm

package token

import "strings"

// The Cloudflare Worker build (GOOS=js GOARCH=wasm) cannot link cgo, so
// gojieba and kagome's IPADIC dictionary are unavailable. This pure-Go
// fallback keeps the same Analyzer API with a dictionary-free segmenter:
// CJK runs are split into 2-char chunks (plus single-char tokens added by
// pushToken), so search still has word-level precision ("中国" does not
// match "中间") though recall/ranking differ from jieba. POS categories are
// heuristic (noun by default, particle/verb endings detected).
//
// The Rust worker (default deployment) keeps the full jieba+lindera pipeline.

// Analyzer is a stateless heuristic segmenter in wasm builds.
type Analyzer struct{}

func New() *Analyzer { return &Analyzer{} }

func (a *Analyzer) Close() {}

// ---- Chinese ----

func (a *Analyzer) zhTokenize(text string, out []string) []string {
	for _, t := range heuristicWords(text, 0) {
		out = pushToken(out, t.W)
	}
	return out
}

// jaTokenize is a no-op: heuristicWords already covers kana runs, and
// Tokenize dedupes the union.
func (a *Analyzer) jaTokenize(_ string, out []string) []string { return out }

func (a *Analyzer) ZhTag(text string) []Tok { return a.zhTag(text) }

func (a *Analyzer) ZhTagHMM(text string) []Tok { return a.zhTag(text) }

func (a *Analyzer) zhTag(text string) []Tok {
	var out []Tok
	forEachChunk(text, segChunkChars, func(base int, chunk string) {
		out = append(out, heuristicWords(chunk, base)...)
	})
	return out
}

func hasCJKRune(rs []rune) bool {
	for _, r := range rs {
		if isCJK(r) {
			return true
		}
	}
	return false
}

// heuristicWords emits word tokens for a chunk: ASCII/other runs are single
// tokens, CJK runs are 2-char chunks (a trailing single char when odd).
func heuristicWords(text string, base int) []Tok {
	runes := []rune(text)
	var out []Tok
	i := 0
	for i < len(runes) {
		if !isAlnum(runes[i]) {
			i++
			continue
		}
		start := i
		for i < len(runes) && isAlnum(runes[i]) {
			i++
		}
		seg := runes[start:i]
		if !hasCJKRune(seg) {
			out = append(out, Tok{W: string(seg), Tag: "n", S: base + start, E: base + i})
			continue
		}
		for j := 0; j < len(seg); j += 2 {
			end := j + 2
			if end > len(seg) {
				end = len(seg)
			}
			w := string(seg[j:end])
			out = append(out, Tok{W: w, Tag: heurTag(w), S: base + start + j, E: base + start + end})
		}
	}
	return out
}

// heurTag guesses a display category without a dictionary: particle-like
// endings (的/地/得) become "u", aspect endings (了/着/过) become "v",
// everything else is a noun.
func heurTag(w string) string {
	if strings.ContainsAny(w, "的地得") {
		return "u"
	}
	if strings.HasSuffix(w, "了") || strings.HasSuffix(w, "着") || strings.HasSuffix(w, "过") {
		return "v"
	}
	return "n"
}

// ---- Japanese ----

// JaTokens is empty in wasm builds; heuristicWords already annotates kana
// runs through ZhTag.
func (a *Analyzer) JaTokens(_ string) []Tok { return nil }
