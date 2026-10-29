package token

import (
	"strings"
	"testing"
)

func TestZhTagOffsets(t *testing.T) {
	a := New()
	defer a.Close()
	toks := a.ZhTag("我来到北京清华大学")
	if len(toks) == 0 {
		t.Fatal("no tokens")
	}
	// verify char offsets cover the text consistently
	text := []rune("我来到北京清华大学")
	for _, tok := range toks {
		if tok.S < 0 || tok.E > len(text) || tok.S >= tok.E {
			t.Fatalf("bad span %+v", tok)
		}
		got := string(text[tok.S:tok.E])
		if got != tok.W {
			t.Fatalf("offset mismatch: token %q != text slice %q", tok.W, got)
		}
	}
	if toks[0].W != "我" {
		t.Fatalf("unexpected first token %q", toks[0].W)
	}
}

func TestZhTagAdjectiveCorrection(t *testing.T) {
	a := New()
	defer a.Close()
	var found bool
	for _, tok := range a.ZhTag("美丽的风景") {
		if tok.W == "美丽" && tok.Tag != "a" {
			t.Fatalf("美丽 should be corrected to a, got %q", tok.Tag)
		}
		if tok.W == "美丽" {
			found = true
		}
	}
	if !found {
		t.Fatal("美丽 not segmented")
	}
}

func TestJaTokensKana(t *testing.T) {
	a := New()
	defer a.Close()
	toks := a.JaTokens("すもももももももものうち")
	if len(toks) == 0 {
		t.Fatal("no ja tokens")
	}
	var kana int
	for _, tok := range toks {
		if HasKana(tok.W) {
			kana++
		}
		if !strings.Contains(tok.Tag, ",") {
			t.Fatalf("expected joined POS fields, got %q", tok.Tag)
		}
	}
	if kana == 0 {
		t.Fatal("expected kana-bearing tokens")
	}
}

func TestAnnotateMarkdownSkipsCode(t *testing.T) {
	a := New()
	defer a.Close()
	out := a.AnnotateMarkdown("你好 world\n\n```\n你好\n```\n")
	if !strings.Contains(out, `cat-`) {
		t.Fatal("expected annotation spans")
	}
	// the 你好 inside the code fence must not be annotated
	fence := strings.Index(out, "```")
	if fence < 0 {
		t.Fatal("fence missing")
	}
	rest := out[fence:]
	if strings.Contains(rest[:strings.Index(rest, "```\n")+4], "posw") {
		t.Fatalf("code fence content was annotated: %q", rest)
	}
}

func TestTokenizeIncludesSingleCJK(t *testing.T) {
	a := New()
	defer a.Close()
	toks := a.Tokenize("故宫")
	joined := strings.Join(toks, " ")
	for _, want := range []string{"故宫", "故", "宫"} {
		if !strings.Contains(joined, want) {
			t.Fatalf("missing token %q in %q", want, joined)
		}
	}
}

func TestAnnotateIdempotent(t *testing.T) {
	a := New()
	defer a.Close()
	once := a.AnnotateMarkdown("慢慢走")
	twice := a.AnnotateMarkdown(once)
	if once != twice {
		t.Fatalf("not idempotent:\n%s\n%s", once, twice)
	}
}
