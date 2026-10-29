package api

import (
	"encoding/json"
	"net/http"
	"sort"
	"strings"

	"urlnote/goback/internal/analyze"
	"urlnote/goback/internal/token"
	"urlnote/goback/shared"
)

// ---- grammar/morphology analysis & POS endpoints ----

// posMaxChars mirrors POS_MAX_CHARS in the Rust worker.
const posMaxChars = 30000

func (s *Server) analyzeText(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.requireUser(w, r); !ok {
		return
	}
	var body shared.AnalyzeRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	writeJSON(w, http.StatusOK, shared.AnalyzeResponse{
		Issues: analyze.Analyze(s.Ana, body.Content),
	})
}

func (s *Server) posTag(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.requireUser(w, r); !ok {
		return
	}
	var body shared.AnalyzeRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}

	runes := []rune(body.Content)
	if len(runes) > posMaxChars {
		runes = runes[:posMaxChars]
	}
	text := string(runes)
	var tokens []shared.PosToken
	for _, t := range s.Ana.ZhTag(text) {
		// punctuation labeled "x" is noise — skip it. NOTE: only actual
		// punctuation ranges (fullwidth CJK punctuation lives in U+3000..U+303F),
		// never "anything above U+2000" — that would eat hanzi.
		if strings.HasPrefix(t.Tag, "x") && allPunctRunes(t.W) {
			continue
		}
		// ZhTag already normalized the tag through zh_cat + the correction
		// table — the tag IS the category
		tokens = append(tokens, shared.PosToken{W: t.W, Cat: t.Tag, S: t.S, E: t.E})
	}
	for _, t := range s.Ana.JaTokens(text) {
		// the IPADIC segmenter mangles pure-han (Chinese) runs — keep only
		// kana-bearing tokens here; jieba owns the Chinese side
		if !token.HasKana(t.W) {
			continue
		}
		cat, ok := jaCatDisplay(t.Tag)
		if !ok {
			continue
		}
		tokens = append(tokens, shared.PosToken{W: t.W, Cat: cat, S: t.S, E: t.E})
	}
	sort.SliceStable(tokens, func(i, j int) bool {
		if tokens[i].S != tokens[j].S {
			return tokens[i].S < tokens[j].S
		}
		return tokens[i].E < tokens[j].E
	})
	writeJSON(w, http.StatusOK, shared.PosResponse{
		Tokens:      tokens,
		TaggedChars: len(runes),
	})
}

// jaCatDisplay mirrors the category mapping inlined in the Rust pos handler.
func jaCatDisplay(pos string) (string, bool) {
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

// allPunctRunes mirrors the pos endpoint's punctuation test: every char is
// ASCII punctuation or fullwidth CJK punctuation (U+3000..U+303F).
func allPunctRunes(w string) bool {
	for _, r := range w {
		ascii := (r >= '!' && r <= '/') || (r >= ':' && r <= '@') ||
			(r >= '[' && r <= '`') || (r >= '{' && r <= '~')
		if !ascii && !(r >= 0x3000 && r <= 0x303f) {
			return false
		}
	}
	return true
}

func (s *Server) posDocument(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.requireUser(w, r); !ok {
		return
	}
	var body shared.AnalyzeRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	writeJSON(w, http.StatusOK, shared.PosDocResponse{
		Markdown: s.Ana.AnnotateMarkdown(body.Content),
	})
}
