// Package analyze ports the Rust worker's analyze.rs: rule-based
// grammar/morphology analysis for Chinese and Japanese, built on the same
// segmenters that power search (jieba POS tags + IPADIC).
//
// These are deterministic lint rules, not a full grammar engine — they catch
// the common, high-confidence mistakes (duplicated words/punctuation,
// 的/地/得 misuse, doubled particles, mixed polite/plain Japanese styles,
// unbalanced brackets) and always pair a problem with a concrete correction
// when one exists.
//
// Offsets are char offsets (both segmenters emit them relative to the full
// text) so the client can apply suggestions by spreading the string into
// code points.
package analyze

import (
	"fmt"
	"sort"
	"strings"

	"urlnote/goback/internal/token"
	"urlnote/goback/shared"
)

func isPunct(r rune) bool {
	switch r {
	case ',', '。', '、', '，', '．', '！', '？', '；', '：', '!', '?', ';', ':', '.':
		return true
	}
	return false
}

// ---- general rules (both languages) ----

func pushDupIssue(issues *[]shared.Issue, word string, startChar, endChar int) {
	var problem string
	if len([]rune(word)) == 1 {
		problem = fmt.Sprintf("「%s」连续出现", word)
	} else {
		problem = fmt.Sprintf("词语「%s」重复", word)
	}
	*issues = append(*issues, shared.Issue{
		Start: startChar, End: endChar,
		Quote:      word + word,
		Problem:    problem,
		Suggestion: word,
	})
}

func allPunct(s string) bool {
	for _, r := range s {
		if !isPunct(r) {
			return false
		}
	}
	return true
}

func checkDuplicatedWords(a *token.Analyzer, issues *[]shared.Issue, text string) {
	// dictionary-based pass: single-char and >=3-char repeats are almost
	// always mistakes; 2-char dictionary words may be legitimate ABAB
	// reduplication (研究研究), so they are skipped here
	toks := a.ZhTag(text)
	for i := 0; i+1 < len(toks); i++ {
		x, y := toks[i], toks[i+1]
		n := len([]rune(x.W))
		if x.W == y.W && x.W != "" && !allPunct(x.W) && (n == 1 || n >= 3) {
			pushDupIssue(issues, x.W, x.S, y.E)
		}
	}
	// HMM pass: repeats among out-of-vocabulary words (e.g. 很好很好) are
	// not dictionary reduplication and worth flagging
	hmm := a.ZhTagHMM(text)
	for i := 0; i+1 < len(hmm); i++ {
		x, y := hmm[i], hmm[i+1]
		if x.W == y.W && x.Tag == "i" && x.W != "" && !allPunct(x.W) && (x.S != y.S || x.E != y.E) {
			pushDupIssue(issues, x.W, x.S, y.E)
		}
	}
}

func checkPunctuation(issues *[]shared.Issue, text string) {
	chars := []rune(text)
	i := 0
	for i+1 < len(chars) {
		c, d := chars[i], chars[i+1]
		if c == d && isPunct(c) {
			j := i + 1
			for j < len(chars) && chars[j] == c {
				j++
			}
			*issues = append(*issues, shared.Issue{
				Start:      i,
				End:        j - 1 + 1,
				Quote:      string(chars[i:j]),
				Problem:    "标点符号重复",
				Suggestion: string(c),
			})
			i = j
		} else if isPunct(c) && isPunct(d) && c != d && !(c == '.' && d == '.') {
			// mixed punctuation pair like "，。" usually a slip; suggest the
			// stronger terminal mark
			keep := c
			if d == '。' || d == '！' || d == '？' {
				keep = d
			}
			*issues = append(*issues, shared.Issue{
				Start:      i,
				End:        i + 2,
				Quote:      string([]rune{c, d}),
				Problem:    "连续使用两个不同的标点",
				Suggestion: string(keep),
			})
			i += 2
		} else {
			i++
		}
	}
}

func checkRepeatedNgrams(a *token.Analyzer, issues *[]shared.Issue, text string) {
	tags := a.ZhTag(text)
	// NOTE: kept structurally identical to the Rust original, including its
	// char-vs-byte offset comparison — zh_tag spans are CHAR offsets while
	// byteStart is a BYTE offset, so for CJK text the ABAB-verb exemption
	// below effectively never fires (a latent quirk of the Rust code, ported
	// verbatim for behavioral parity).
	isABABVerb := func(byteStart int, gram string) bool {
		if len([]rune(gram)) != 2 {
			return false
		}
		for _, t := range tags {
			if t.S == byteStart && t.E == byteStart+len(gram) && (t.Tag == "v" || t.Tag == "vn") {
				return true
			}
		}
		return false
	}
	chars := []rune(text)
	byteAt := make([]int, len(chars))
	b := 0
	for i, r := range chars {
		byteAt[i] = b
		b += len(string(r))
	}
	type span struct{ s, e int }
	var reported []span
	for _, n := range []int{4, 3, 2} {
		if len(chars) < 2*n {
			continue
		}
		i := 0
		for i+2*n <= len(chars) {
			ok := true
			for k := 0; k < n; k++ {
				if isPunct(chars[i+k]) || chars[i+k] == ' ' || chars[i+k] == '\t' || chars[i+k] == '\n' || chars[i+k] == '\r' {
					ok = false
					break
				}
			}
			repeated := ok
			if repeated {
				for k := 0; k < n; k++ {
					if chars[i+k] != chars[i+n+k] {
						repeated = false
						break
					}
				}
			}
			if repeated {
				if n == 2 && isABABVerb(byteAt[i], string(chars[i:i+n])) {
					i += 2 * n
					continue
				}
				key := span{i, i + 2*n}
				covered := false
				for _, r := range reported {
					if key.s >= r.s && key.e <= r.e {
						covered = true
						break
					}
				}
				if !covered {
					reported = append(reported, key)
					seg := string(chars[i : i+n])
					*issues = append(*issues, shared.Issue{
						Start:      i,
						End:        i + 2*n,
						Quote:      seg + seg,
						Problem:    fmt.Sprintf("「%s」重复出现", seg),
						Suggestion: seg,
					})
				}
				i += 2 * n
			} else {
				i++
			}
		}
	}
}

func checkBrackets(issues *[]shared.Issue, text string) {
	pairs := [][2]rune{{'（', '）'}, {'(', ')'}, {'「', '」'}, {'『', '』'}, {'【', '】'}, {'《', '》'}}
	for _, p := range pairs {
		open, close := p[0], p[1]
		opens := strings.Count(text, string(open))
		closes := strings.Count(text, string(close))
		if opens != closes {
			var problem string
			if opens > closes {
				problem = fmt.Sprintf("「%c」比「%c」多 %d 处", open, close, opens-closes)
			} else {
				problem = fmt.Sprintf("「%c」比「%c」多 %d 处", close, open, closes-opens)
			}
			*issues = append(*issues, shared.Issue{
				Problem:    problem,
				Suggestion: fmt.Sprintf("请配对使用 %c%c", open, close),
			})
		}
	}
}

// ---- Chinese rules (jieba POS tags) ----

// checkDeDi catches 的/地/得 misuse, the classic trio:
//   - 形容词 + 的 + 动词 → 地 (慢慢地走)
//   - 动词 + 的 + 副/形 → 得 (跑得很快)
//   - 形容词 + 地 + 名词 → 的 (美丽的风景)
func checkDeDi(a *token.Analyzer, issues *[]shared.Issue, text string) {
	tags := a.ZhTag(text)
	fix := func(m token.Tok, want, why string) {
		*issues = append(*issues, shared.Issue{
			Start:      m.S,
			End:        m.E,
			Quote:      m.W,
			Problem:    fmt.Sprintf("「%s」应为「%s」:%s", m.W, want, why),
			Suggestion: want,
			Lang:       "zh",
		})
	}
	for i := 0; i+2 < len(tags); i++ {
		x, m, y := tags[i], tags[i+1], tags[i+2]
		aux := m.W
		if m.Tag != "uj" && aux != "的" && aux != "地" && aux != "得" {
			continue
		}
		adjOrAdv := x.Tag == "a" || x.Tag == "d"
		switch aux {
		case "的":
			if adjOrAdv && y.Tag == "v" {
				fix(m, "地", "形容词修饰动词作状语应用「地」")
			} else if x.Tag == "v" && (y.Tag == "d" || y.Tag == "a") {
				fix(m, "得", "动词后接程度补语应用「得」")
			}
		case "地":
			if adjOrAdv && y.Tag == "n" {
				fix(m, "的", "形容词修饰名词应用「的」")
			}
		}
	}
	// jieba often keeps 「慢慢的」 whole instead of splitting off 「的」 — catch
	// word-final 的/地/得 on adjectives/verbs via the surrounding tokens
	for i := 0; i+1 < len(tags); i++ {
		x, y := tags[i], tags[i+1]
		r := []rune(x.W)
		if len(r) == 0 || (r[len(r)-1] != '的' && r[len(r)-1] != '地' && r[len(r)-1] != '得') {
			continue
		}
		last := r[len(r)-1]
		tailStart := x.E - 1 // char offsets: the last rune is 1 char
		var corrected string
		switch {
		case (x.Tag == "a" || x.Tag == "d") && y.Tag == "v" && last == '的':
			corrected = string(r[:len(r)-1]) + "地"
			*issues = append(*issues, shared.Issue{
				Start: tailStart, End: x.E, Quote: x.W,
				Problem:    "「的」应为「地」:形容词修饰动词作状语应用「地」",
				Suggestion: corrected,
				Lang:       "zh",
			})
		case x.Tag == "v" && (y.Tag == "d" || y.Tag == "a") && last == '的':
			corrected = string(r[:len(r)-1]) + "得"
			*issues = append(*issues, shared.Issue{
				Start: tailStart, End: x.E, Quote: x.W,
				Problem:    "「的」应为「得」:动词后接程度补语应用「得」",
				Suggestion: corrected,
				Lang:       "zh",
			})
		case x.Tag == "a" && y.Tag == "n" && last == '地':
			corrected = string(r[:len(r)-1]) + "的"
			*issues = append(*issues, shared.Issue{
				Start: tailStart, End: x.E, Quote: x.W,
				Problem:    "「地」应为「的」:形容词修饰名词应用「的」",
				Suggestion: corrected,
				Lang:       "zh",
			})
		}
	}
}

// ---- Japanese rules (IPADIC POS) ----

func isParticle(pos string) bool {
	return strings.HasPrefix(pos, "助詞")
}

// checkJaParticles catches doubled particles: "をを" "にに" "のの" and
// particle runs like "にはは".
func checkJaParticles(a *token.Analyzer, issues *[]shared.Issue, text string) {
	toks := a.JaTokens(text)
	for i := 0; i+1 < len(toks); i++ {
		x, y := toks[i], toks[i+1]
		if isParticle(x.Tag) && isParticle(y.Tag) {
			if x.W == y.W {
				*issues = append(*issues, shared.Issue{
					Start: x.S, End: y.E,
					Quote:      x.W + y.W,
					Problem:    fmt.Sprintf("助詞「%s」重复", x.W),
					Suggestion: x.W,
					Lang:       "ja",
				})
			} else {
				*issues = append(*issues, shared.Issue{
					Start: x.S, End: y.E,
					Quote:      x.W + y.W,
					Problem:    "两个助詞连续出现，请确认语序",
					Suggestion: x.W + y.W,
					Lang:       "ja",
				})
			}
		}
	}
}

// checkJaStyle catches mixed polite (です・ます) and plain (だ・である)
// endings in one passage.
func checkJaStyle(a *token.Analyzer, issues *[]shared.Issue, text string) {
	toks := a.JaTokens(text)
	var polite, plain int
	for _, t := range toks {
		if strings.HasPrefix(t.Tag, "助動詞") || strings.HasPrefix(t.Tag, "動詞") || strings.HasPrefix(t.Tag, "形容詞") {
			switch t.W {
			case "です", "ます", "でした", "ました", "ません":
				polite++
			case "だ", "である", "だった", "だっ":
				plain++
			}
		}
	}
	if polite >= 2 && plain >= 1 {
		*issues = append(*issues, shared.Issue{
			Problem:    fmt.Sprintf("です・ます调(%d 处)与だ・である调(%d 处)混用,建议统一文体", polite, plain),
			Suggestion: "统一为です・ます调或だ・である调",
			Lang:       "ja",
		})
	}
}

// Analyze returns all detected issues in text, ordered by position.
func Analyze(a *token.Analyzer, text string) []shared.Issue {
	var issues []shared.Issue
	if strings.TrimSpace(text) == "" {
		return issues
	}
	checkPunctuation(&issues, text)
	checkRepeatedNgrams(a, &issues, text)
	checkDuplicatedWords(a, &issues, text)
	checkBrackets(&issues, text)
	checkDeDi(a, &issues, text)
	checkJaParticles(a, &issues, text)
	checkJaStyle(a, &issues, text)
	sort.SliceStable(issues, func(i, j int) bool {
		if issues[i].Start != issues[j].Start {
			return issues[i].Start < issues[j].Start
		}
		return issues[i].End < issues[j].End
	})
	// dedupe identical (start, end, suggestion) entries
	out := issues[:0]
	for i, iss := range issues {
		if i > 0 && iss.Start == issues[i-1].Start && iss.End == issues[i-1].End && iss.Suggestion == issues[i-1].Suggestion {
			continue
		}
		out = append(out, iss)
	}
	issues = out
	if len(issues) > 100 {
		issues = issues[:100]
	}
	return issues
}
